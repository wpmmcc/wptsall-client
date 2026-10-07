use anyhow::Context;
use reqwest::Method;
use serde_json::json;

use super::*;

pub(super) fn component_transport_error(
    stage: &str,
    component_id: &str,
    error: reqwest::Error,
) -> anyhow::Error {
    let kind = if error.is_timeout() {
        "timeout"
    } else {
        "transport"
    };
    // reqwest's source chain can contain the original URL, even when an
    // outer context has redacted it. Do not attach that chain to diagnostics.
    anyhow!(
        "{} failed (component={}, kind={})",
        stage,
        component_id,
        kind
    )
}

pub(super) async fn invoke_component_api(
    client: &Client,
    runtime: &ComponentRuntime,
    request_spec: &ComponentRequest,
    rendered_url: &str,
    ctx: &HashMap<String, String>,
    sign_result: SignResult,
) -> anyhow::Result<Value> {
    invoke_component_api_with_source(
        client,
        runtime,
        request_spec,
        rendered_url,
        ctx,
        sign_result,
        None,
    )
    .await
}

pub(super) async fn invoke_component_api_with_source(
    client: &Client,
    runtime: &ComponentRuntime,
    request_spec: &ComponentRequest,
    rendered_url: &str,
    ctx: &HashMap<String, String>,
    sign_result: SignResult,
    source: Option<SourceRequest<'_>>,
) -> anyhow::Result<Value> {
    let budget = HttpBudget::new(
        request_spec.http_limits.as_ref(),
        HttpResponseKind::Json,
        &runtime.template.id,
    )?;
    budget
        .wait(invoke_component_api_with_budget(
            client,
            runtime,
            request_spec,
            rendered_url,
            ctx,
            sign_result,
            source,
            &budget,
        ))
        .await?
}

#[allow(clippy::too_many_arguments)]
async fn invoke_component_api_with_budget(
    client: &Client,
    runtime: &ComponentRuntime,
    request_spec: &ComponentRequest,
    rendered_url: &str,
    ctx: &HashMap<String, String>,
    sign_result: SignResult,
    source: Option<SourceRequest<'_>>,
    budget: &HttpBudget<'_>,
) -> anyhow::Result<Value> {
    let method = Method::from_bytes(request_spec.method.as_bytes())
        .with_context(|| format!("unsupported method: {}", request_spec.method))?;
    let url = rendered_url;
    assert_provider_url_allowed(url)?;

    let mut request = client.request(method, url);

    // GAP-06 收尾: run 级 trace 挂在供应商请求上（含 mock 消费面）——
    // 组件模板头与签名头在其后追加，不冲突；无 run 时不挂，保持线上兼容。
    if let Some(trace_id) = crate::auth::current_run_trace_id() {
        request = request.header(crate::config::TRACE_ID_HEADER, trace_id);
    }

    let body_type = request_spec.body_type.as_deref().unwrap_or("json");

    if let Some(headers) = &request_spec.headers {
        for (k, v) in headers {
            // For multipart requests, let reqwest set boundary Content-Type.
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

    // Apply SignResult headers
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
                let form = value_to_multipart_form_with_source(
                    client,
                    runtime
                        .template
                        .constraints
                        .as_ref()
                        .and_then(|c| c.max_file_size_mb)
                        .unwrap_or(0),
                    &rendered,
                    source,
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

    let response = request.send().await.map_err(|error| {
        component_transport_error("component request", &runtime.template.id, error)
    })?;
    assert_provider_redirect_origin(url, response.url())?;
    // N-2 (07 audit, 12 批 A3): with provider_redirect_policy armed, a
    // stopped-at-30x response means a cross-origin redirect was refused —
    // report it explicitly instead of letting the redirect body fail later
    // as an opaque parse error.
    if response.status().is_redirection() {
        return Err(anyhow!(
            "provider redirect crossed host boundary (not followed)"
        ));
    }
    let status = response.status();
    let retry_after_ms = response
        .headers()
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(|secs| secs.saturating_mul(1000));
    if !status.is_success() {
        let retry_after_note = retry_after_ms
            .map(|v| format!(", retry_after_ms={}", v))
            .unwrap_or_default();
        return Err(anyhow!(
            "component api non-2xx (status={}, component={}{})",
            status,
            runtime.template.id,
            retry_after_note
        ));
    }

    let bytes = budget.read(response).await?;
    // reqwest's charset feature is disabled in the shared products.
    let body_text = String::from_utf8_lossy(&bytes);
    let body_json: Value =
        serde_json::from_str(&body_text).unwrap_or_else(|_| json!({ "body": body_text }));

    // Some providers return HTTP 200 for logical/auth errors and place details
    // under response.error_path. Detect and surface them as runtime errors
    // before translation output extraction.
    if let Some(error_path) = runtime.template.response.error_path.as_deref() {
        if extract_component_logical_error(&body_json, error_path).is_some() {
            return Err(anyhow!(
                "component api logical error (status={}, component={}, error_path='{}')",
                status,
                runtime.template.id,
                error_path
            ));
        }
    }

    Ok(body_json)
}

#[derive(Debug, Clone)]
pub(super) struct BinaryResponseAsset {
    pub(super) bytes: Vec<u8>,
    pub(super) filename: Option<String>,
}

pub(super) async fn invoke_component_api_binary(
    client: &Client,
    runtime: &ComponentRuntime,
    request_spec: &ComponentRequest,
    rendered_url: &str,
    ctx: &HashMap<String, String>,
    sign_result: SignResult,
) -> anyhow::Result<BinaryResponseAsset> {
    invoke_component_api_binary_with_source(
        client,
        runtime,
        request_spec,
        rendered_url,
        ctx,
        sign_result,
        None,
    )
    .await
}

pub(super) async fn invoke_component_api_binary_with_source(
    client: &Client,
    runtime: &ComponentRuntime,
    request_spec: &ComponentRequest,
    rendered_url: &str,
    ctx: &HashMap<String, String>,
    sign_result: SignResult,
    source: Option<SourceRequest<'_>>,
) -> anyhow::Result<BinaryResponseAsset> {
    let budget = HttpBudget::new(
        request_spec.http_limits.as_ref(),
        HttpResponseKind::Binary,
        &runtime.template.id,
    )?;
    budget
        .wait(invoke_component_api_binary_with_budget(
            client,
            runtime,
            request_spec,
            rendered_url,
            ctx,
            sign_result,
            source,
            &budget,
        ))
        .await?
}

#[allow(clippy::too_many_arguments)]
async fn invoke_component_api_binary_with_budget(
    client: &Client,
    runtime: &ComponentRuntime,
    request_spec: &ComponentRequest,
    rendered_url: &str,
    ctx: &HashMap<String, String>,
    sign_result: SignResult,
    source: Option<SourceRequest<'_>>,
    budget: &HttpBudget<'_>,
) -> anyhow::Result<BinaryResponseAsset> {
    let method = Method::from_bytes(request_spec.method.as_bytes())
        .with_context(|| format!("unsupported method: {}", request_spec.method))?;
    let url = rendered_url;
    assert_provider_url_allowed(url)?;

    let mut request = client.request(method, url);

    // GAP-06 收尾: run 级 trace（同 invoke_component_api，二进制资产路径）。
    if let Some(trace_id) = crate::auth::current_run_trace_id() {
        request = request.header(crate::config::TRACE_ID_HEADER, trace_id);
    }

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
                let form = value_to_multipart_form_with_source(
                    client,
                    runtime
                        .template
                        .constraints
                        .as_ref()
                        .and_then(|c| c.max_file_size_mb)
                        .unwrap_or(0),
                    &rendered,
                    source,
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

    let response = request.send().await.map_err(|error| {
        component_transport_error("component request", &runtime.template.id, error)
    })?;
    assert_provider_redirect_origin(url, response.url())?;
    // N-2 (07 audit, 12 批 A3): with provider_redirect_policy armed, a
    // stopped-at-30x response means a cross-origin redirect was refused —
    // report it explicitly instead of letting the redirect body fail later
    // as an opaque parse error.
    if response.status().is_redirection() {
        return Err(anyhow!(
            "provider redirect crossed host boundary (not followed)"
        ));
    }
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!(
            "component api non-2xx (status={}, component={})",
            status,
            runtime.template.id
        ));
    }
    let filename = response
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let idx = v.to_ascii_lowercase().find("filename=")?;
            let rest = v[idx + "filename=".len()..].trim();
            let trimmed = rest.trim_matches('"').trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
    let body_bytes = budget.read(response).await?;

    Ok(BinaryResponseAsset {
        bytes: body_bytes,
        filename,
    })
}

pub(super) fn extract_component_logical_error(value: &Value, error_path: &str) -> Option<String> {
    let v = extract_json_path(value, error_path)?;
    match v {
        Value::Null => None,
        Value::Bool(false) => Some("false".to_string()),
        Value::Bool(true) => None,
        Value::Number(n) => {
            let n_str = n.to_string();
            if n_str == "0" || n_str == "200" {
                None
            } else {
                Some(n_str)
            }
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty()
                || trimmed == "0"
                || trimmed.eq_ignore_ascii_case("ok")
                || trimmed.eq_ignore_ascii_case("success")
                || trimmed.eq_ignore_ascii_case("succeeded")
            {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Array(_) => None,
        Value::Object(_) => None,
    }
}

pub(super) fn extract_non_text_translated_ref_heuristic(
    value: &Value,
    task_type: &str,
) -> Option<String> {
    let mut keys: Vec<&str> = Vec::new();
    match normalize_task_content_type(task_type).as_str() {
        "image" => keys.extend(["translated_image_ref", "image_ref", "image_url"]),
        "video" => keys.extend(["translated_video_ref", "video_ref", "video_url"]),
        "audio" => keys.extend(["translated_audio_ref", "audio_ref", "audio_url"]),
        "document" => keys.extend([
            "translated_document_ref",
            "document_ref",
            "document_url",
            "file_ref",
            "file_url",
        ]),
        _ => {}
    }
    keys.extend([
        "translated_media_ref",
        "media_ref",
        "translated_ref",
        "ref",
        "url",
        "url_download",
    ]);

    let prefixes = [
        "",
        "data",
        "result",
        "output",
        "payload",
        "data.result",
        "data.output",
        "data.payload",
        "result.data",
    ];
    for prefix in prefixes {
        for key in &keys {
            let path = if prefix.is_empty() {
                (*key).to_string()
            } else {
                format!("{}.{}", prefix, key)
            };
            if let Some(raw) = extract_json_path_string(value, &path) {
                let candidate = raw.trim();
                if !candidate.is_empty() {
                    return Some(candidate.to_string());
                }
            }
        }
    }
    find_nested_ref_candidate(value, &keys, 0)
}

fn find_nested_ref_candidate(value: &Value, keys: &[&str], depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(raw) = map.get(*key).and_then(extract_candidate_string) {
                    return Some(raw);
                }
            }
            for child in map.values() {
                if let Some(found) = find_nested_ref_candidate(child, keys, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => {
            for child in items {
                if let Some(found) = find_nested_ref_candidate(child, keys, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_candidate_string(value: &Value) -> Option<String> {
    match value {
        Value::String(raw) => {
            let candidate = raw.trim();
            if candidate.is_empty() {
                None
            } else {
                Some(candidate.to_string())
            }
        }
        Value::Number(raw) => Some(raw.to_string()),
        Value::Bool(raw) => Some(raw.to_string()),
        _ => None,
    }
}

pub(super) fn extract_non_text_translated_text_heuristic(value: &Value) -> Option<String> {
    let keys = [
        "translated_text",
        "text",
        "translated",
        "caption",
        "description",
        "transcript",
        "title",
    ];
    let prefixes = [
        "data",
        "result",
        "output",
        "data.result",
        "data.output",
        "result.data",
    ];
    for prefix in prefixes {
        for key in &keys {
            let path = format!("{}.{}", prefix, key);
            if let Some(raw) = extract_json_path_string(value, &path) {
                let candidate = raw.trim();
                if !candidate.is_empty() {
                    return Some(candidate.to_string());
                }
            }
        }
    }
    None
}

pub(super) fn extract_json_path_string(value: &Value, path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let v = extract_json_path(value, trimmed)?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if v.is_number() || v.is_boolean() {
        return Some(v.to_string());
    }
    None
}

pub(super) fn render_template_value(value: &Value, ctx: &HashMap<String, String>) -> Value {
    match value {
        Value::String(s) => {
            if let Some(token_key) = single_template_token_key(s) {
                if token_key.starts_with("computed.") {
                    if let Some(raw) = ctx.get(token_key) {
                        if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                            if parsed.is_array() || parsed.is_object() {
                                return parsed;
                            }
                        }
                    }
                }
            }
            Value::String(render_template_string(s, ctx))
        }
        Value::Array(arr) => {
            Value::Array(arr.iter().map(|v| render_template_value(v, ctx)).collect())
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), render_template_value(v, ctx));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

pub(super) fn inject_rendered_request_context(
    request_spec: &ComponentRequest,
    rendered_url: &str,
    ctx: &mut HashMap<String, String>,
) {
    ctx.insert("request.method".to_string(), request_spec.method.clone());
    ctx.insert("request.url".to_string(), rendered_url.to_string());

    let Some(body) = &request_spec.body else {
        return;
    };

    let rendered = render_template_value(body, ctx);
    let rendered_json = serde_json::to_string(&rendered).unwrap_or_default();
    if !rendered_json.is_empty() {
        ctx.insert("request.body_json".to_string(), rendered_json);
    }
    if let Some(obj) = rendered.as_object() {
        for (key, value) in obj {
            let rendered_value = match value {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                Value::Bool(_) | Value::Number(_) => value.to_string(),
                Value::Array(_) | Value::Object(_) => {
                    serde_json::to_string(value).unwrap_or_default()
                }
            };
            ctx.insert(format!("request.body.{}", key), rendered_value);
        }
    }
}

fn single_template_token_key(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    if !trimmed.starts_with("{{") || !trimmed.ends_with("}}") {
        return None;
    }
    let inner = trimmed[2..trimmed.len().saturating_sub(2)].trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner)
    }
}

pub(super) fn render_template_string(input: &str, ctx: &HashMap<String, String>) -> String {
    let mut output = input.to_string();
    for (key, value) in ctx {
        let token = format!("{{{{{}}}}}", key);
        output = output.replace(&token, value);
    }
    output
}

pub(super) fn resolve_template_json_path(
    path: &str,
    ctx: &HashMap<String, String>,
) -> Option<String> {
    let rendered = render_template_string(path, ctx);
    let trimmed = rendered.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn extract_json_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        if part.is_empty() {
            continue;
        }
        match current {
            Value::Object(map) => {
                current = map.get(part)?;
            }
            Value::Array(arr) => {
                let idx = part.parse::<usize>().ok()?;
                current = arr.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

pub(super) fn apply_language_map(
    language_map: &HashMap<String, String>,
    ctx: &mut HashMap<String, String>,
) {
    if language_map.is_empty() {
        return;
    }
    // Update both input.* and payload.* keys so components reading from either
    // namespace get the mapped language codes.
    for key in [
        "input.source_lang",
        "input.target_lang",
        "payload.source_lang",
        "payload.target_lang",
    ] {
        if let Some(lang_code) = ctx.get(key).cloned() {
            if let Some(mapped) = language_map.get(&lang_code) {
                ctx.insert(key.to_string(), mapped.clone());
            } else {
                // Try prefix: "en_US" -> "en"
                let prefix = lang_code.split('_').next().unwrap_or(&lang_code);
                if let Some(mapped) = language_map.get(prefix) {
                    ctx.insert(key.to_string(), mapped.clone());
                }
            }
        }
    }
}

pub(super) fn value_to_string_map(value: &serde_json::Value) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(arr) => serde_json::to_string(arr).unwrap_or_default(),
                other => other.to_string(),
            };
            map.insert(k.clone(), s);
        }
    }
    map
}

pub(super) async fn value_to_multipart_form_async(
    client: &Client,
    max_file_size_mb: u32,
    value: &serde_json::Value,
) -> anyhow::Result<reqwest::multipart::Form> {
    value_to_multipart_form_with_source(client, max_file_size_mb, value, None).await
}

pub(super) async fn value_to_multipart_form_with_source(
    client: &Client,
    max_file_size_mb: u32,
    value: &serde_json::Value,
    source: Option<SourceRequest<'_>>,
) -> anyhow::Result<reqwest::multipart::Form> {
    let mut form = reqwest::multipart::Form::new();

    let mut max_bytes = if max_file_size_mb > 0 {
        (max_file_size_mb as u64)
            .saturating_mul(1024)
            .saturating_mul(1024)
    } else {
        0
    };
    if let Some(source) = source {
        max_bytes = crate::component_rt::file_limits::strictest(max_bytes, source.byte_budget);
    }

    let Some(obj) = value.as_object() else {
        return Ok(form.text("payload".to_string(), value.to_string()));
    };

    for (k, v) in obj {
        let rendered = match v {
            serde_json::Value::String(s) => s.trim().to_string(),
            serde_json::Value::Array(arr) => serde_json::to_string(arr).unwrap_or_default(),
            other => other.to_string(),
        };

        // Special syntax: "@file:<url|data-url>" uploads the referenced bytes as a file part.
        let file_ref = rendered
            .strip_prefix("@file:")
            .or_else(|| rendered.strip_prefix("@file_url:"))
            .map(str::trim);

        if let Some(file_ref) = file_ref {
            let asset = match source
                .filter(|source| source.reference.trim() == file_ref)
                .and_then(|source| source.verified)
            {
                Some(asset) => std::borrow::Cow::Borrowed(asset),
                None => {
                    std::borrow::Cow::Owned(fetch_source_asset(client, file_ref, max_bytes).await?)
                }
            };
            anyhow::ensure!(
                max_bytes == 0 || u64::try_from(asset.bytes.len())? <= max_bytes,
                "multipart source exceeds selected credential byte budget"
            );
            let content_bytes = asset.bytes.clone();
            let filename = asset.filename.clone();
            let mut part =
                reqwest::multipart::Part::bytes(content_bytes.clone()).file_name(filename.clone());
            if is_reasonable_mime_string(&asset.content_type) {
                part = part
                    .mime_str(&asset.content_type)
                    .or_else(|_| {
                        reqwest::multipart::Part::bytes(content_bytes.clone())
                            .file_name(filename.clone())
                            .mime_str("application/octet-stream")
                    })
                    .map_err(|err| {
                        anyhow::anyhow!(
                            "failed to build multipart part with mime '{}': {err}",
                            asset.content_type
                        )
                    })?;
            }
            form = form.part(k.to_string(), part);
        } else {
            form = form.text(k.to_string(), rendered);
        }
    }

    Ok(form)
}
