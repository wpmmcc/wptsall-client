use anyhow::{anyhow, Context};
use reqwest::{Client, Method};
use serde_json::Value;
use std::collections::HashMap;

use crate::logging::snippet;

use super::signing::prime_sign_context;
use super::*;

pub(super) fn normalize_status_value(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

pub(super) fn normalize_status_values(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let normalized = normalize_status_value(v);
        if normalized.is_empty() || out.contains(&normalized) {
            continue;
        }
        out.push(normalized);
    }
    out
}

pub(super) fn status_values_contains(values: &[String], status: &str) -> bool {
    let normalized = normalize_status_value(status);
    values.iter().any(|v| v == &normalized)
}

pub(super) fn apply_async_poll_submit_extract(
    async_poll: &ComponentAsyncPoll,
    submit_json: &Value,
    ctx: &mut HashMap<String, String>,
    component_id: &str,
) -> anyhow::Result<()> {
    apply_json_extract_map(
        &async_poll.submit_extract,
        submit_json,
        ctx,
        component_id,
        "async_poll.submit_extract",
    )
}

pub(super) fn apply_prepare_extract(
    prepare: &ComponentPrepare,
    prepare_json: &Value,
    ctx: &mut HashMap<String, String>,
    component_id: &str,
) -> anyhow::Result<()> {
    apply_json_extract_map(
        &prepare.extract,
        prepare_json,
        ctx,
        component_id,
        "prepare.extract",
    )
}

pub(super) fn apply_source_upload_extract(
    source_upload: &ComponentSourceUpload,
    upload_json: &Value,
    ctx: &mut HashMap<String, String>,
    component_id: &str,
) -> anyhow::Result<()> {
    apply_json_extract_map(
        &source_upload.extract,
        upload_json,
        ctx,
        component_id,
        "source_upload.extract",
    )
}

pub(super) fn apply_json_extract_map(
    extract_map: &HashMap<String, String>,
    response_json: &Value,
    ctx: &mut HashMap<String, String>,
    component_id: &str,
    field_label: &str,
) -> anyhow::Result<()> {
    if extract_map.is_empty() {
        return Ok(());
    }
    for (raw_key, raw_path) in extract_map {
        let key = raw_key.trim();
        if key.is_empty() {
            continue;
        }
        let path = raw_path.trim();
        if path.is_empty() {
            return Err(anyhow!(
                "{} '{}' has empty json path (component={})",
                field_label,
                key,
                component_id
            ));
        }

        let extracted = extract_json_path(response_json, path);
        let value = extracted
            .map(|v| match v {
                Value::Null => String::new(),
                Value::String(s) => s.trim().to_string(),
                Value::Bool(_) | Value::Number(_) => v.to_string(),
                Value::Array(_) | Value::Object(_) => v.to_string(),
            })
            .unwrap_or_default();

        if value.trim().is_empty() {
            let body_preview: String = response_json.to_string().chars().take(200).collect();
            return Err(anyhow!(
                "{} '{}' path '{}' not found in response (component={}, body_preview={})",
                field_label,
                key,
                path,
                component_id,
                body_preview
            ));
        }

        let ctx_key = if key.starts_with("computed.") {
            key.to_string()
        } else {
            format!("computed.{}", key)
        };
        ctx.insert(ctx_key, value);
    }
    Ok(())
}

fn extract_translated_text_best_effort(body_json: &Value, path: &str) -> Option<String> {
    let translated_value = extract_json_path(body_json, path.trim())?;
    let translated = if let Some(v) = translated_value.as_str() {
        v.to_string()
    } else {
        translated_value.to_string()
    };
    if translated.trim().is_empty() {
        None
    } else {
        Some(translated)
    }
}

pub(super) async fn call_component_request_json(
    client: &Client,
    runtime: &ComponentRuntime,
    request_spec: &ComponentRequest,
    ctx: &mut HashMap<String, String>,
) -> anyhow::Result<Value> {
    let _runtime_concurrency_guard = acquire_runtime_concurrency_guard(runtime).await?;
    enforce_runtime_rate_limit(runtime).await;
    prime_sign_context(runtime.template.sign.as_ref(), ctx)?;
    let rendered_url = render_template_string(&request_spec.url, ctx);
    inject_rendered_request_context(request_spec, &rendered_url, ctx);
    let sign_result = process_sign_config(
        runtime.template.sign.as_ref(),
        ctx,
        &request_spec.method,
        &rendered_url,
    )?;
    invoke_component_api(
        client,
        runtime,
        request_spec,
        &rendered_url,
        ctx,
        sign_result,
    )
    .await
}

#[derive(Debug, Clone)]
pub(super) struct ExtractedNonTextOutcome {
    pub(super) translated_ref: String,
    pub(super) translated_text: String,
}

pub(super) fn extract_non_text_outcome_best_effort(
    runtime: &ComponentRuntime,
    body_json: &Value,
    normalized_task_type: &str,
    source_ref: &str,
    allow_source_fallback: bool,
    translated_ref_path_override: Option<&str>,
) -> ExtractedNonTextOutcome {
    let mut translated_text = runtime
        .template
        .response
        .translated_text_path
        .as_deref()
        .and_then(|path| extract_translated_text_best_effort(body_json, path))
        .unwrap_or_default();
    if translated_text.trim().is_empty() {
        translated_text = extract_non_text_translated_text_heuristic(body_json).unwrap_or_default();
    }

    let mut translated_ref_candidates: Vec<String> = Vec::new();
    let mut candidate_paths: Vec<&str> = Vec::new();
    if let Some(path) = translated_ref_path_override
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        candidate_paths.push(path);
    }
    if let Some(path) = runtime.template.response.translated_ref_path.as_deref() {
        candidate_paths.push(path);
    }
    match normalized_task_type {
        "image" => {
            if let Some(path) = runtime
                .template
                .response
                .translated_image_ref_path
                .as_deref()
            {
                candidate_paths.push(path);
            }
        }
        "video" => {
            if let Some(path) = runtime
                .template
                .response
                .translated_video_ref_path
                .as_deref()
            {
                candidate_paths.push(path);
            }
        }
        "audio" => {
            if let Some(path) = runtime
                .template
                .response
                .translated_audio_ref_path
                .as_deref()
            {
                candidate_paths.push(path);
            }
        }
        "document" => {
            if let Some(path) = runtime
                .template
                .response
                .translated_document_ref_path
                .as_deref()
            {
                candidate_paths.push(path);
            }
        }
        "mixed" => {
            if let Some(path) = runtime
                .template
                .response
                .translated_media_ref_path
                .as_deref()
            {
                candidate_paths.push(path);
            }
            if let Some(path) = runtime
                .template
                .response
                .translated_image_ref_path
                .as_deref()
            {
                candidate_paths.push(path);
            }
            if let Some(path) = runtime
                .template
                .response
                .translated_video_ref_path
                .as_deref()
            {
                candidate_paths.push(path);
            }
            if let Some(path) = runtime
                .template
                .response
                .translated_audio_ref_path
                .as_deref()
            {
                candidate_paths.push(path);
            }
            if let Some(path) = runtime
                .template
                .response
                .translated_document_ref_path
                .as_deref()
            {
                candidate_paths.push(path);
            }
        }
        _ => {}
    }
    if let Some(path) = runtime
        .template
        .response
        .translated_media_ref_path
        .as_deref()
    {
        candidate_paths.push(path);
    }

    for path in candidate_paths {
        if let Some(value) = extract_json_path_string(body_json, path) {
            let value = value.trim();
            if !value.is_empty() {
                translated_ref_candidates.push(value.to_string());
            }
        }
    }
    if let Some(value) = extract_non_text_translated_ref_heuristic(body_json, normalized_task_type)
    {
        translated_ref_candidates.push(value);
    }

    let translated_ref = resolve_non_text_translated_ref(
        &translated_ref_candidates,
        source_ref,
        &translated_text,
        allow_source_fallback,
    );

    ExtractedNonTextOutcome {
        translated_ref,
        translated_text,
    }
}

pub(super) fn finalize_non_text_outcome(
    ctx: &HashMap<String, String>,
    runtime: &ComponentRuntime,
    normalized_task_type: &str,
    outcome: ExtractedNonTextOutcome,
) -> anyhow::Result<NonTextComponentOutcome> {
    if outcome.translated_ref.trim().is_empty() && outcome.translated_text.trim().is_empty() {
        return Err(anyhow!(
            "non-text output path not found (task_type={}, component={})",
            normalized_task_type,
            runtime.template.id
        ));
    }

    let translated_ref = if !outcome.translated_ref.trim().is_empty()
        && !outcome.translated_ref.starts_with("http://")
        && !outcome.translated_ref.starts_with("https://")
        && !outcome.translated_ref.starts_with("data:")
        && looks_like_base64_blob(&outcome.translated_ref)
    {
        let mime = ctx
            .get("input.source_mime")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or("application/octet-stream");
        format!("data:{};base64,{}", mime, outcome.translated_ref.trim())
    } else {
        outcome.translated_ref
    };

    Ok(NonTextComponentOutcome {
        translated_ref,
        translated_text: outcome.translated_text,
    })
}

#[derive(Debug, Clone)]
pub(super) struct DownloadedBinaryAsset {
    pub(super) bytes: Vec<u8>,
    pub(super) filename: Option<String>,
}

pub(super) async fn call_component_request_binary(
    client: &Client,
    runtime: &ComponentRuntime,
    request_spec: &ComponentAsyncDownload,
    ctx: &mut HashMap<String, String>,
) -> anyhow::Result<DownloadedBinaryAsset> {
    let _runtime_concurrency_guard = acquire_runtime_concurrency_guard(runtime).await?;
    enforce_runtime_rate_limit(runtime).await;

    prime_sign_context(runtime.template.sign.as_ref(), ctx)?;
    let rendered_url = render_template_string(&request_spec.url, ctx);
    let sign_result = process_sign_config(
        runtime.template.sign.as_ref(),
        ctx,
        &request_spec.method,
        &rendered_url,
    )?;

    let method = Method::from_bytes(request_spec.method.as_bytes())
        .with_context(|| format!("unsupported method: {}", request_spec.method))?;
    let mut request = client.request(method, &rendered_url);
    assert_provider_url_allowed(&rendered_url)?;

    let body_type = request_spec.body_type.as_deref().unwrap_or("json");

    if let Some(headers) = &request_spec.headers {
        for (k, v) in headers {
            if body_type == "multipart" && k.eq_ignore_ascii_case("content-type") {
                continue;
            }
            let rendered = render_template_string(v, ctx);
            if rendered.trim().is_empty() {
                continue;
            }
            request = request.header(k, rendered);
        }
    }

    match &sign_result {
        SignResult::ContextOnly(_) => {}
        SignResult::WithHeaders(_, headers) => {
            for (k, v) in headers {
                if v.trim().is_empty() {
                    continue;
                }
                request = request.header(k, v);
            }
        }
        SignResult::AuthorizationHeader(value) => {
            if !value.trim().is_empty() {
                request = request.header("Authorization", value);
            }
        }
    }

    if let Some(body) = &request_spec.body {
        match body_type {
            "form" => {
                let rendered = render_template_value(body, ctx);
                let form_map = value_to_string_map(&rendered);
                request = request.form(&form_map);
            }
            "multipart" => {
                let rendered = render_template_value(body, ctx);
                let form = value_to_multipart_form_async(
                    client,
                    runtime
                        .template
                        .constraints
                        .as_ref()
                        .and_then(|c| c.max_file_size_mb)
                        .unwrap_or(0),
                    &rendered,
                )
                .await?;
                request = request.multipart(form);
            }
            "none" => {}
            _ => {
                request = request.json(&render_template_value(body, ctx));
            }
        }
    }

    let safe_url = redact_url_secrets(&rendered_url);
    let response = request
        .send()
        .await
        .with_context(|| format!("component request failed ({})", safe_url))?;
    assert_provider_redirect_origin(&rendered_url, response.url())?;
    let status = response.status();

    let filename = response
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split("filename=").nth(1))
        .map(|v| v.trim().trim_matches('\"').to_string())
        .filter(|v| !v.is_empty());

    let body_bytes = response
        .bytes()
        .await
        .with_context(|| format!("component response read failed ({})", safe_url))?
        .to_vec();

    if !status.is_success() {
        let body_preview = snippet(&String::from_utf8_lossy(&body_bytes));
        return Err(anyhow!(
            "component api non-2xx (status={}, body_preview={})",
            status,
            body_preview
        ));
    }

    Ok(DownloadedBinaryAsset {
        bytes: body_bytes,
        filename,
    })
}

pub(super) async fn finalize_non_text_async(
    client: &Client,
    runtime: &ComponentRuntime,
    ctx: &mut HashMap<String, String>,
    normalized_task_type: &str,
    source_ref: &str,
    poll_json: &Value,
) -> anyhow::Result<NonTextComponentOutcome> {
    let async_poll = runtime.template.async_poll.as_ref().ok_or_else(|| {
        anyhow!(
            "async_poll is not configured (component={})",
            runtime.template.id
        )
    })?;

    let mut translated_text = runtime
        .template
        .response
        .translated_text_path
        .as_deref()
        .and_then(|path| extract_translated_text_best_effort(poll_json, path))
        .unwrap_or_default();
    if translated_text.trim().is_empty() {
        translated_text = extract_non_text_translated_text_heuristic(poll_json).unwrap_or_default();
    }

    let rendered_result_ref_path = async_poll
        .result_ref_path
        .as_deref()
        .and_then(|path| resolve_template_json_path(path, ctx));
    let extracted_ref = extract_non_text_outcome_best_effort(
        runtime,
        poll_json,
        normalized_task_type,
        source_ref,
        false,
        rendered_result_ref_path.as_deref(),
    )
    .translated_ref;
    if !extracted_ref.trim().is_empty() {
        ctx.insert("computed.result_ref".to_string(), extracted_ref.clone());
        ctx.insert("computed.result_url".to_string(), extracted_ref);
    }

    if let Some(download) = async_poll.result_download.as_ref() {
        let asset = call_component_request_binary(client, runtime, download, ctx).await?;
        let preferred_name = download
            .filename
            .as_deref()
            .map(|s| render_template_string(s, ctx))
            .filter(|v| !v.trim().is_empty())
            .or_else(|| asset.filename.clone());
        let local_path = persist_downloaded_asset_to_temp(&asset.bytes, preferred_name.as_deref())?;
        let translated_ref = format!("file://{}", local_path);
        return finalize_non_text_outcome(
            ctx,
            runtime,
            normalized_task_type,
            ExtractedNonTextOutcome {
                translated_ref,
                translated_text,
            },
        );
    }

    if let Some(result_request) = async_poll.result_request.as_ref() {
        let result_json = call_component_request_json(client, runtime, result_request, ctx).await?;
        let rendered_result_ref_path = async_poll
            .result_ref_path
            .as_deref()
            .and_then(|path| resolve_template_json_path(path, ctx));
        let outcome = extract_non_text_outcome_best_effort(
            runtime,
            &result_json,
            normalized_task_type,
            source_ref,
            false,
            rendered_result_ref_path.as_deref(),
        );
        let outcome = ExtractedNonTextOutcome {
            translated_ref: outcome.translated_ref,
            translated_text,
        };
        return finalize_non_text_outcome(ctx, runtime, normalized_task_type, outcome);
    }

    if let Some(tpl) = async_poll
        .result_ref_template
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let translated_ref = render_template_string(tpl, ctx);
        return finalize_non_text_outcome(
            ctx,
            runtime,
            normalized_task_type,
            ExtractedNonTextOutcome {
                translated_ref,
                translated_text,
            },
        );
    }

    let rendered_result_ref_path = async_poll
        .result_ref_path
        .as_deref()
        .and_then(|path| resolve_template_json_path(path, ctx));
    let outcome = extract_non_text_outcome_best_effort(
        runtime,
        poll_json,
        normalized_task_type,
        source_ref,
        false,
        rendered_result_ref_path.as_deref(),
    );
    let outcome = ExtractedNonTextOutcome {
        translated_ref: outcome.translated_ref,
        translated_text,
    };
    finalize_non_text_outcome(ctx, runtime, normalized_task_type, outcome)
}
