use anyhow::Context;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use aws_smithy_runtime_api::client::http::{
    http_client_fn, HttpConnector, HttpConnectorFuture, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::result::ConnectorError;
use reqwest::Method;
use serde_json::Value;

use super::signing::prime_sign_context;
use super::*;

#[cfg(test)]
#[path = "../../../../../tests/modules/client-wpplugin/unit/source_asset_limits.rs"]
mod source_asset_limits;

#[cfg(test)]
#[path = "../../../../../tests/modules/client-wpplugin/unit/s3_source_transport.rs"]
mod s3_source_transport;

#[cfg(test)]
#[path = "../../../../../tests/modules/client-wpplugin/unit/encrypted_provider_assets.rs"]
mod encrypted_provider_assets;

#[derive(Debug, Clone)]
pub(super) struct SourceAsset {
    pub(super) bytes: Vec<u8>,
    pub(super) content_type: String,
    pub(super) filename: String,
}

#[derive(Clone, Copy)]
pub(super) struct SourceRequest<'a> {
    pub(super) reference: &'a str,
    pub(super) verified: Option<&'a SourceAsset>,
    pub(super) byte_budget: u64,
}

/// Fetch the Content-Length of a remote file via HEAD request (returns bytes).
/// Returns Err if the request fails or Content-Length header is absent/unparseable.
pub(super) async fn get_file_content_length(client: &Client, url: &str) -> anyhow::Result<u64> {
    let resp = client
        .head(url)
        .send()
        .await
        .context("HEAD request for file size check")?;
    resp.headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| anyhow::anyhow!("no Content-Length in HEAD response"))
}

fn strip_content_type_params(raw: &str) -> String {
    raw.split(';').next().unwrap_or(raw).trim().to_string()
}

pub(super) fn is_reasonable_mime_string(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() {
        return false;
    }
    if !s.is_ascii() || s.chars().any(char::is_whitespace) {
        return false;
    }
    let mut parts = s.split('/');
    let a = parts.next().unwrap_or_default();
    let b = parts.next().unwrap_or_default();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if parts.next().is_some() {
        return false;
    }
    true
}

pub(super) fn looks_like_base64_blob(raw: &str) -> bool {
    let s = raw.trim();
    if s.len() < 128 {
        return false;
    }
    if s.chars().any(char::is_whitespace) {
        return false;
    }
    s.is_ascii()
        && s.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '='))
}

fn parse_base64_data_url(data_url: &str, max_bytes: u64) -> anyhow::Result<(String, Vec<u8>)> {
    let invalid = || anyhow!("unsupported data url (expected data:*;base64,...)");
    let s = data_url.trim();
    let rest = s.strip_prefix("data:").ok_or_else(invalid)?;
    let comma = rest.find(',').ok_or_else(invalid)?;
    let (meta, payload) = rest.split_at(comma);
    let payload = payload.trim_start_matches(',').trim();
    if payload.is_empty() {
        return Err(invalid());
    }
    let mut content_type = "application/octet-stream";
    let mut is_b64 = false;
    for part in meta.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        if part.eq_ignore_ascii_case("base64") {
            is_b64 = true;
        } else if !part.contains('=') {
            content_type = part;
        }
    }
    if !is_b64 {
        return Err(invalid());
    }
    if payload.len() % 4 != 0 {
        return Err(invalid());
    }
    let padding = if payload.ends_with("==") {
        2
    } else if payload.ends_with('=') {
        1
    } else {
        0
    };
    let decoded_len = (payload.len() / 4)
        .checked_mul(3)
        .and_then(|length| length.checked_sub(padding))
        .ok_or_else(invalid)?;
    let decoded_bytes =
        u64::try_from(decoded_len).context("data URL source byte count does not fit u64")?;
    if max_bytes > 0 && decoded_bytes > max_bytes {
        return Err(anyhow!(
            "source asset too large ({} bytes > {} bytes)",
            decoded_bytes,
            max_bytes
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(decoded_len)
        .context("allocate data URL source buffer failed")?;
    bytes.resize(decoded_len, 0);
    let written = BASE64_STANDARD
        .decode_slice(payload, &mut bytes)
        .map_err(|_| invalid())?;
    anyhow::ensure!(written == decoded_len, "invalid data URL source byte count");
    Ok((strip_content_type_params(content_type), bytes))
}

fn filename_from_url(url: &str) -> String {
    let base = url
        .split('#')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url);
    base.rsplit('/')
        .next()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("source_file")
        .trim()
        .to_string()
}

fn filename_from_content_disposition(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let idx = lower.find("filename=")?;
    let rest = raw[idx + "filename=".len()..].trim();
    if rest.is_empty() {
        return None;
    }
    let trimmed = rest.trim_start_matches('"');
    let end = trimmed
        .find('"')
        .or_else(|| trimmed.find(';'))
        .unwrap_or(trimmed.len());
    let name = trimmed[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

pub(super) async fn fetch_source_asset(
    client: &Client,
    source_ref: &str,
    max_bytes: u64,
) -> anyhow::Result<SourceAsset> {
    let src = source_ref.trim();
    if src.is_empty() {
        return Err(anyhow!("source_ref is empty"));
    }

    if src.starts_with("data:") {
        let (content_type, bytes) = parse_base64_data_url(src, max_bytes)?;
        return Ok(SourceAsset {
            bytes,
            content_type,
            filename: "source_file".to_string(),
        });
    }

    if !(src.starts_with("http://") || src.starts_with("https://")) {
        return Err(anyhow!(
            "unsupported source_ref scheme (need http/https or data url)"
        ));
    }

    assert_provider_url_allowed(src)?;

    let mut resp =
        client.get(src).send().await.map_err(|error| {
            component_transport_error("download source_ref", "source_asset", error)
        })?;
    assert_provider_redirect_origin(src, resp.url())?;
    if !resp.status().is_success() {
        return Err(anyhow!(
            "download source_ref failed (status={})",
            resp.status()
        ));
    }

    if max_bytes > 0 {
        if let Some(cl) = resp.content_length() {
            if cl > max_bytes {
                return Err(anyhow!(
                    "source asset too large ({} bytes > {} bytes)",
                    cl,
                    max_bytes
                ));
            }
        }
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(strip_content_type_params)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "application/octet-stream".to_string());

    let filename = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(filename_from_content_disposition)
        .unwrap_or_else(|| filename_from_url(src));

    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|error| {
        component_transport_error("read source_ref bytes", "source_asset", error)
    })? {
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow!("source asset byte count overflow"))?;
        let actual_bytes =
            u64::try_from(next_len).context("source asset byte count does not fit u64")?;
        if max_bytes > 0 && actual_bytes > max_bytes {
            return Err(anyhow!(
                "source asset too large ({} bytes > {} bytes)",
                actual_bytes,
                max_bytes
            ));
        }
        bytes
            .try_reserve(chunk.len())
            .context("allocate source asset buffer failed")?;
        bytes.extend_from_slice(&chunk);
    }

    Ok(SourceAsset {
        bytes,
        content_type,
        filename,
    })
}

fn template_needs_source_asset(template: &ComponentTemplate) -> bool {
    let mut haystack = String::new();
    haystack.push_str(&template.request.url);
    if let Some(headers) = &template.request.headers {
        for v in headers.values() {
            haystack.push_str(v);
        }
    }
    if let Some(body) = &template.request.body {
        haystack.push_str(&body.to_string());
    }
    haystack.contains("source_base64")
        || haystack.contains("source_mime")
        || haystack.contains("source_filename")
}

pub(super) async fn maybe_insert_non_text_source_asset_context(
    client: &Client,
    runtime: &ComponentRuntime,
    ctx: &mut HashMap<String, String>,
    source_ref: &str,
    verified_source: Option<&SourceAsset>,
) -> anyhow::Result<()> {
    if source_ref.trim().is_empty() {
        return Ok(());
    }
    if !template_needs_source_asset(&runtime.template) {
        return Ok(());
    }
    if ctx.contains_key("input.source_base64") || ctx.contains_key("payload.source_base64") {
        return Ok(());
    }

    let max_mb = runtime
        .template
        .constraints
        .as_ref()
        .and_then(|c| c.max_file_size_mb)
        .unwrap_or(0) as u64;
    let max_bytes = if max_mb > 0 {
        max_mb.saturating_mul(1024).saturating_mul(1024)
    } else {
        0
    };

    let asset = match verified_source {
        Some(asset) => std::borrow::Cow::Borrowed(asset),
        None => std::borrow::Cow::Owned(fetch_source_asset(client, source_ref, max_bytes).await?),
    };
    let b64 = BASE64_STANDARD.encode(&asset.bytes);

    ctx.insert("input.source_base64".to_string(), b64.clone());
    ctx.insert("payload.source_base64".to_string(), b64);
    ctx.insert("input.source_mime".to_string(), asset.content_type.clone());
    ctx.insert(
        "payload.source_mime".to_string(),
        asset.content_type.clone(),
    );
    ctx.insert("input.source_filename".to_string(), asset.filename.clone());
    ctx.insert(
        "payload.source_filename".to_string(),
        asset.filename.clone(),
    );
    Ok(())
}

pub(super) async fn ensure_non_text_source_asset_metadata_context(
    client: &Client,
    runtime: &ComponentRuntime,
    ctx: &mut HashMap<String, String>,
    source_ref: &str,
    verified_source: Option<&SourceAsset>,
) -> anyhow::Result<()> {
    if source_ref.trim().is_empty() {
        return Ok(());
    }
    if ctx.contains_key("input.source_filename") && ctx.contains_key("input.source_mime") {
        return Ok(());
    }

    let max_mb = runtime
        .template
        .constraints
        .as_ref()
        .and_then(|c| c.max_file_size_mb)
        .unwrap_or(0) as u64;
    let max_bytes = if max_mb > 0 {
        max_mb.saturating_mul(1024).saturating_mul(1024)
    } else {
        0
    };

    let asset = match verified_source {
        Some(asset) => std::borrow::Cow::Borrowed(asset),
        None => std::borrow::Cow::Owned(fetch_source_asset(client, source_ref, max_bytes).await?),
    };
    ctx.insert("input.source_mime".to_string(), asset.content_type.clone());
    ctx.insert(
        "payload.source_mime".to_string(),
        asset.content_type.clone(),
    );
    ctx.insert("input.source_filename".to_string(), asset.filename.clone());
    ctx.insert(
        "payload.source_filename".to_string(),
        asset.filename.clone(),
    );
    Ok(())
}

pub(super) async fn upload_source_asset_to_vendor(
    client: &Client,
    runtime: &ComponentRuntime,
    source_upload: &ComponentSourceUpload,
    ctx: &mut HashMap<String, String>,
    source_ref: &str,
    verified_source: Option<&SourceAsset>,
) -> anyhow::Result<()> {
    let budget = HttpBudget::new(
        source_upload.http_limits.as_ref(),
        HttpResponseKind::UploadAck,
        &runtime.template.id,
    )?;
    budget
        .wait(async {
            let method =
                Method::from_bytes(source_upload.method.as_bytes()).with_context(|| {
                    format!("unsupported source_upload method: {}", source_upload.method)
                })?;
            prime_sign_context(runtime.template.sign.as_ref(), ctx)?;
            let rendered_url = render_template_string(&source_upload.url, ctx);
            let sign_result = process_sign_config(
                runtime.template.sign.as_ref(),
                ctx,
                &source_upload.method,
                &rendered_url,
            )?;

            let max_mb = runtime
                .template
                .constraints
                .as_ref()
                .and_then(|c| c.max_file_size_mb)
                .unwrap_or(0) as u64;
            let max_bytes = if max_mb > 0 {
                max_mb.saturating_mul(1024).saturating_mul(1024)
            } else {
                0
            };
            let asset = match verified_source {
                Some(asset) => std::borrow::Cow::Borrowed(asset),
                None => std::borrow::Cow::Owned(
                    fetch_source_asset(client, source_ref, max_bytes).await?,
                ),
            };
            ctx.insert("input.source_mime".to_string(), asset.content_type.clone());
            ctx.insert(
                "payload.source_mime".to_string(),
                asset.content_type.clone(),
            );
            ctx.insert("input.source_filename".to_string(), asset.filename.clone());
            ctx.insert(
                "payload.source_filename".to_string(),
                asset.filename.clone(),
            );

            let body_type = source_upload
                .body_type
                .as_deref()
                .unwrap_or("binary_source")
                .trim()
                .to_ascii_lowercase();
            let mut configured_content_type: Option<String> = None;
            let mut rendered_headers: Vec<(String, String)> = Vec::new();

            if let Some(headers) = &source_upload.headers {
                for (k, v) in headers {
                    let rendered = render_template_string(v, ctx);
                    let s3_routing_header = body_type == "aws_s3_put_object"
                        && (k.eq_ignore_ascii_case("x-wptsall-s3-upload-profile")
                            || k.eq_ignore_ascii_case("x-amz-endpoint-url"));
                    if rendered.trim().is_empty() && !s3_routing_header {
                        continue;
                    }
                    if k.eq_ignore_ascii_case("content-type") {
                        configured_content_type = Some(rendered.clone());
                    }
                    rendered_headers.push((k.clone(), rendered));
                }
            }

            if body_type == "aws_s3_put_object" {
                upload_source_asset_to_s3_with_budget(
                    client,
                    runtime,
                    &rendered_url,
                    &rendered_headers,
                    &asset,
                    &budget,
                )
                .await?;
                return Ok(());
            }

            assert_provider_url_allowed(&rendered_url)?;

            let mut request = client.request(method, &rendered_url);
            for (k, rendered) in &rendered_headers {
                request = request.header(k, rendered);
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

            if body_type != "binary_source" {
                return Err(anyhow!(
                    "unsupported source_upload.body_type '{}' (component={})",
                    body_type,
                    runtime.template.id
                ));
            }

            request = request
                .header(
                    reqwest::header::CONTENT_TYPE,
                    configured_content_type
                        .filter(|v| !v.trim().is_empty())
                        .unwrap_or_else(|| asset.content_type.clone()),
                )
                .body(asset.bytes.clone());

            let response = request.send().await.map_err(|error| {
                component_transport_error("source_upload request", &runtime.template.id, error)
            })?;
            assert_provider_redirect_origin(&rendered_url, response.url())?;
            let status = response.status();
            let expected_statuses = &source_upload.success_statuses;
            let success = if expected_statuses.is_empty() {
                status.is_success()
            } else {
                expected_statuses.contains(&status.as_u16())
            };
            if !success {
                return Err(anyhow!(
                    "source_upload failed (status={}, component={})",
                    status,
                    runtime.template.id
                ));
            }

            let bytes = budget.read(response).await?;
            let body = String::from_utf8_lossy(&bytes);
            if !source_upload.extract.is_empty() {
                let body_json: Value = serde_json::from_str(&body).map_err(|_| {
                    anyhow!(
                        "source_upload response is not json (component={})",
                        runtime.template.id
                    )
                })?;
                apply_source_upload_extract(source_upload, &body_json, ctx, &runtime.template.id)?;
            }

            Ok(())
        })
        .await?
}

fn parse_s3_upload_target(rendered_url: &str) -> anyhow::Result<(String, String)> {
    let raw = rendered_url.trim();
    let target = raw.strip_prefix("s3://").ok_or_else(|| {
        anyhow!("aws_s3_put_object requires source_upload.url to start with s3://")
    })?;
    let mut parts = target.splitn(2, '/');
    let bucket = parts.next().unwrap_or_default().trim();
    let key = parts.next().unwrap_or_default().trim();
    if bucket.is_empty() || key.is_empty() {
        return Err(anyhow!(
            "aws_s3_put_object requires s3://bucket/key source_upload.url"
        ));
    }
    Ok((bucket.to_string(), key.to_string()))
}

fn lookup_rendered_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum S3UploadProfile {
    AwsS3,
    PrivateMock,
}

fn s3_upload_profile(headers: &[(String, String)]) -> anyhow::Result<S3UploadProfile> {
    let mut values = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("x-wptsall-s3-upload-profile"));
    let Some((_, value)) = values.next() else {
        return Ok(S3UploadProfile::AwsS3);
    };
    if values.next().is_some() {
        return Err(anyhow!("duplicate S3 source upload profile"));
    }
    match value.trim() {
        "aws_s3" => Ok(S3UploadProfile::AwsS3),
        "private_mock" => Ok(S3UploadProfile::PrivateMock),
        _ => Err(anyhow!("invalid S3 source upload profile")),
    }
}

fn private_mock_s3_url(endpoint: &str, bucket: &str, key: &str) -> anyhow::Result<String> {
    let mut endpoint =
        url::Url::parse(endpoint).map_err(|_| anyhow!("invalid private S3 mock endpoint"))?;
    let loopback = match endpoint.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address
            .to_ipv4_mapped()
            .map(|address| address.is_loopback())
            .unwrap_or_else(|| address.is_loopback()),
        _ => false,
    };
    if !loopback || endpoint.query().is_some() || endpoint.fragment().is_some() {
        return Err(anyhow!(
            "private S3 mock requires an explicit loopback endpoint"
        ));
    }
    assert_provider_url_allowed(endpoint.as_str())?;
    endpoint
        .path_segments_mut()
        .map_err(|_| anyhow!("invalid private S3 mock endpoint"))?
        .pop_if_empty()
        .push("mock-upload")
        .push(bucket)
        .extend(key.split('/'));
    Ok(endpoint.into())
}

#[derive(Clone)]
struct SourceUploadConnector {
    client: Client,
    budget: HttpBudget<'static>,
}

impl std::fmt::Debug for SourceUploadConnector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SourceUploadConnector")
    }
}

fn source_upload_transport_error(message: &'static str) -> ConnectorError {
    ConnectorError::other(Box::new(std::io::Error::other(message)), None)
}

impl HttpConnector for SourceUploadConnector {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let client = self.client.clone();
        let budget = self.budget;
        HttpConnectorFuture::new(async move {
            assert_provider_url_allowed(request.uri())
                .map_err(|_| source_upload_transport_error("S3 request URL rejected"))?;
            let method = Method::from_bytes(request.method().as_bytes())
                .map_err(|_| source_upload_transport_error("S3 request method invalid"))?;
            let uri = request.uri().to_string();
            let mut headers = reqwest::header::HeaderMap::new();
            for (name, value) in request.headers().iter() {
                let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| source_upload_transport_error("S3 request header invalid"))?;
                let value = reqwest::header::HeaderValue::from_str(value)
                    .map_err(|_| source_upload_transport_error("S3 request header invalid"))?;
                headers.append(name, value);
            }
            let body = ByteStream::new(request.into_body())
                .collect()
                .await
                .map_err(|_| source_upload_transport_error("S3 request body failed"))?
                .into_bytes();
            let response = client
                .request(method, uri)
                .headers(headers)
                .body(body)
                .send()
                .await
                .map_err(|_| source_upload_transport_error("S3 upload transport failed"))?;
            let status = response
                .status()
                .as_u16()
                .try_into()
                .map_err(|_| source_upload_transport_error("S3 response status invalid"))?;
            let mut response_headers = Vec::new();
            for (name, value) in response.headers() {
                let value = value
                    .to_str()
                    .map_err(|_| source_upload_transport_error("S3 response header invalid"))?;
                response_headers.push((name.as_str().to_string(), value.to_string()));
            }
            let body = budget
                .read(response)
                .await
                .map_err(|_| source_upload_transport_error("S3 response body failed"))?;
            let mut response = HttpResponse::new(status, body.into());
            for (name, value) in response_headers {
                response
                    .headers_mut()
                    .try_append(name, value)
                    .map_err(|_| source_upload_transport_error("S3 response header invalid"))?;
            }
            Ok(response)
        })
    }
}

#[cfg(test)]
async fn upload_source_asset_to_s3(
    client: &Client,
    runtime: &ComponentRuntime,
    rendered_url: &str,
    rendered_headers: &[(String, String)],
    asset: &SourceAsset,
) -> anyhow::Result<()> {
    let budget = HttpBudget::new(None, HttpResponseKind::UploadAck, &runtime.template.id)?;
    budget
        .wait(upload_source_asset_to_s3_with_budget(
            client,
            runtime,
            rendered_url,
            rendered_headers,
            asset,
            &budget,
        ))
        .await?
}

async fn upload_source_asset_to_s3_with_budget(
    client: &Client,
    runtime: &ComponentRuntime,
    rendered_url: &str,
    rendered_headers: &[(String, String)],
    asset: &SourceAsset,
    budget: &HttpBudget<'_>,
) -> anyhow::Result<()> {
    let (bucket, key) = parse_s3_upload_target(rendered_url)?;
    let profile = s3_upload_profile(rendered_headers)?;
    if rendered_headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("x-amz-endpoint-url"))
        .count()
        > 1
    {
        return Err(anyhow!("duplicate S3 source upload endpoint"));
    }
    let endpoint_url = lookup_rendered_header(rendered_headers, "x-amz-endpoint-url");
    if endpoint_url.is_none()
        && rendered_headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("x-amz-endpoint-url"))
    {
        return Err(anyhow!("S3 source upload endpoint is empty"));
    }
    let content_type = lookup_rendered_header(rendered_headers, "content-type")
        .unwrap_or(asset.content_type.as_str());
    if profile == S3UploadProfile::PrivateMock {
        let endpoint = endpoint_url
            .ok_or_else(|| anyhow!("private S3 mock requires an explicit loopback endpoint"))?;
        let upload_url = private_mock_s3_url(endpoint, &bucket, &key)?;
        let response = client
            .put(upload_url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(asset.bytes.clone())
            .send()
            .await
            .map_err(|_| anyhow!("private S3 mock upload transport failed"))?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "private S3 mock upload failed (status={})",
                response.status()
            ));
        }
        budget.read(response).await?;
        return Ok(());
    }
    let access_key = lookup_rendered_header(rendered_headers, "x-amz-access-key-id").ok_or_else(
        || {
            anyhow!(
                "aws_s3_put_object requires source_upload.headers.x-amz-access-key-id (component={})",
                runtime.template.id
            )
        },
    )?;
    let secret_key =
        lookup_rendered_header(rendered_headers, "x-amz-secret-access-key").ok_or_else(|| {
            anyhow!(
                "aws_s3_put_object requires source_upload.headers.x-amz-secret-access-key (component={})",
                runtime.template.id
            )
        })?;
    let session_token = lookup_rendered_header(rendered_headers, "x-amz-session-token");
    let region = lookup_rendered_header(rendered_headers, "x-amz-region").unwrap_or("us-east-1");

    // A component may provide an S3-compatible endpoint (MinIO/R2/etc.) via
    // a rendered header. This is still network egress controlled by a
    // template, so it must not bypass the same final SSRF/DNS-rebinding guard
    // used for HTTP provider requests.
    if let Some(endpoint) = endpoint_url {
        assert_provider_url_allowed(endpoint).with_context(|| {
            format!(
                "aws_s3_put_object endpoint rejected (component={})",
                runtime.template.id
            )
        })?;
    }

    let connector = SharedHttpConnector::new(SourceUploadConnector {
        client: client.clone(),
        budget: budget.relabel("source-upload-s3"),
    });
    let http_client = http_client_fn(move |_, _| connector.clone());
    let mut builder = aws_sdk_s3::config::Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .http_client(http_client)
        .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled())
        .region(Region::new(region.to_string()))
        .credentials_provider(Credentials::new(
            access_key,
            secret_key,
            session_token.map(|v| v.to_string()),
            None,
            "component-source-upload",
        ));
    if let Some(endpoint) = endpoint_url {
        builder = builder.endpoint_url(endpoint).force_path_style(true);
    }
    let s3_client = aws_sdk_s3::Client::from_conf(builder.build());
    s3_client
        .put_object()
        .bucket(bucket)
        .key(key)
        .content_type(content_type)
        .body(ByteStream::from(asset.bytes.clone()))
        .send()
        .await
        .map_err(|_| {
            anyhow!(
                "aws_s3_put_object upload failed (component={})",
                runtime.template.id
            )
        })?;
    Ok(())
}

fn sanitize_filename(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "translated_asset".to_string();
    }
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed).trim();
    let mut out = String::new();
    for ch in base.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "translated_asset".to_string()
    } else {
        out
    }
}

pub(super) fn persist_downloaded_provider_asset(
    bytes: &[u8],
    preferred_name: Option<&str>,
) -> anyhow::Result<String> {
    let dir = std::path::PathBuf::from(crate::config::env_or(
        "WPTSALL_DATA_DIR",
        crate::config::DEFAULT_DATA_DIR,
    ))
    .join("provider-assets");
    let dir = crate::retained_assets::directory(&dir)?;
    let filename = preferred_name
        .map(sanitize_filename)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| format!("wptsall-asset-{}.bin", uuid::Uuid::new_v4()));
    let path = dir.join(format!("wpa1{}-{filename}", uuid::Uuid::new_v4().simple()));
    crate::retained_assets::write_bytes(&path, bytes)?;
    Ok(path.to_string_lossy().to_string())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_binary_assets_do_not_overwrite_same_provider_filename() {
        let _key = crate::db::owned_mock_bindings_key();
        let root = tempfile::tempdir().unwrap();
        let _data =
            crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
        let name = format!("owned-durable-{}.bin", uuid::Uuid::new_v4());
        let first = persist_downloaded_provider_asset(b"first", Some(&name)).unwrap();
        let second = persist_downloaded_provider_asset(b"second", Some(&name)).unwrap();
        let first_bytes = crate::retained_assets::read(std::path::Path::new(&first), 5).unwrap();
        let second_bytes = crate::retained_assets::read(std::path::Path::new(&second), 6).unwrap();
        assert_ne!(std::fs::read(&first).unwrap(), b"first");
        assert_ne!(std::fs::read(&second).unwrap(), b"second");
        for path in std::collections::HashSet::from([first.clone(), second.clone()]) {
            std::fs::remove_file(path).unwrap();
        }
        assert_ne!(
            first, second,
            "provider filenames must not overwrite another paid result"
        );
        assert_eq!(first_bytes, b"first");
        assert_eq!(second_bytes, b"second");
        assert!(std::path::Path::new(&first).starts_with(root.path().join("provider-assets")));
    }

    fn minimal_runtime() -> ComponentRuntime {
        ComponentRuntime {
            template: ComponentTemplate {
                id: "s3-guard-test".into(),
                name: "S3 guard test".into(),
                version: "1".into(),
                kind: "translation".into(),
                client_contract: None,
                default_values: None,
                auth: None,
                prepare: None,
                request: ComponentRequest {
                    http_limits: None,
                    method: "POST".into(),
                    url: "https://example.com".into(),
                    headers: None,
                    body: None,
                    body_type: None,
                    response_type: None,
                },
                response: ComponentResponse {
                    translated_text_path: None,
                    translated_ref_path: None,
                    translated_media_ref_path: None,
                    translated_image_ref_path: None,
                    translated_video_ref_path: None,
                    translated_audio_ref_path: None,
                    translated_document_ref_path: None,
                    error_path: None,
                },
                async_poll: None,
                source_upload: None,
                sign: None,
                constraints: None,
                editable_params: Vec::new(),
                translation_modes: Vec::new(),
            },
            auth_values: HashMap::new(),
            supported_business_lines: Vec::new(),
            language_map: HashMap::new(),
            supported_content_formats: Vec::new(),
            supported_formats: Vec::new(),
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
    async fn s3_upload_rejects_private_endpoint_before_sdk_initialization() {
        let runtime = minimal_runtime();
        let asset = SourceAsset {
            bytes: b"fixture".to_vec(),
            content_type: "text/plain".into(),
            filename: "fixture.txt".into(),
        };
        let headers = vec![
            ("x-amz-access-key-id".into(), "AKIA_TEST_REAL".into()),
            ("x-amz-secret-access-key".into(), "secret".into()),
            (
                "x-amz-endpoint-url".into(),
                "http://169.254.169.254:9000".into(),
            ),
        ];
        let err = upload_source_asset_to_s3(
            &Client::new(),
            &runtime,
            "s3://bucket/key",
            &headers,
            &asset,
        )
        .await
        .expect_err("metadata endpoint must be rejected");
        assert!(err.to_string().contains("endpoint rejected"));
    }
}