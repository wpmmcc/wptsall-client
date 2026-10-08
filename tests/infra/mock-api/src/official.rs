use std::collections::HashMap;
use std::fs;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::handlers::{
    auth_error_response, key_enter, key_exit, translate_media_or_default, translate_text,
    translated_media_url,
};
use crate::verify;
use crate::AppState;

#[derive(Clone, Debug)]
pub struct OfficialJob {
    pub family: String,
    pub poll_response: Value,
    pub result_response: Option<Value>,
    pub download_kind: Option<String>,
    pub download_filename: Option<String>,
    pub download_content_type: Option<String>,
    pub poll_count: usize,
}

#[derive(Default, Clone, Debug)]
struct MultipartPayload {
    fields: HashMap<String, String>,
    filenames: HashMap<String, String>,
}

pub async fn handle_official(
    State(state): State<AppState>,
    method: Method,
    uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.0.path().to_string();
    let query = parse_query(uri.0.query());
    let content_type = header_value(&headers, "content-type").unwrap_or_default();
    let body_text = String::from_utf8_lossy(&body).to_string();

    // Per-auth-mode mock profiles (Google key vs OAuth, Azure key vs OAuth, …).
    if let Some(response) =
        crate::profile_handlers::try_handle_profile(&state, &path, &headers, &query, &body_text)
            .await
    {
        return response;
    }

    // Catalog parity paths (former E-gaps + strict auth shapes).
    if let Some(response) =
        crate::catalog_parity::try_handle(&state, &path, &headers, &query, &body_text).await
    {
        return response;
    }

    if path.contains("/chat/completions") {
        return handle_openai_like(&state, &headers, &body_text).await;
    }
    if path.ends_with("/v1/audio/translations") {
        return handle_openai_audio(&state, &headers, &content_type, &body).await;
    }
    if path.ends_with("/v1/messages") {
        return handle_anthropic(&state, &headers, &body_text).await;
    }
    if path.ends_with("/v2/chat") {
        return handle_cohere(&state, &headers, &body_text).await;
    }
    if path.contains(":generateContent") {
        return handle_gemini(&state, &headers, &body_text).await;
    }
    if path == "/v2/translate" && header_value(&headers, "x-api-key").is_some() {
        return handle_translateplus_text(&state, &headers, &body_text).await;
    }
    if path == "/v2/translate" && query.get("key").is_some() {
        return handle_lilt_text(&state, &headers, &body_text).await;
    }
    // DeepL uses `DeepL-Auth-Key`; Matecat/Translated uses Bearer on the same path.
    if path == "/v2/translate" {
        let authorization = header_value(&headers, "authorization").unwrap_or_default();
        if authorization
            .trim()
            .to_ascii_lowercase()
            .starts_with("bearer ")
        {
            return crate::catalog_parity::handle_matecat(&state, &headers, &body_text).await;
        }
        return handle_deepl_text(&state, &headers, &query, &content_type, &body_text).await;
    }
    if path == "/v2/document" {
        return handle_deepl_submit(&state, &headers, &content_type, &body).await;
    }
    if path.starts_with("/v2/document/") {
        return handle_deepl_poll_or_result(&state, &headers, &path).await;
    }
    if path == "/nmt/v1/translation" {
        return handle_papago_text(&state, &headers, &body_text).await;
    }
    if path == "/web-trans/v1/translate" {
        return handle_papago_webpage(&state, &headers, &body_text).await;
    }
    if path == "/image-to-text/v1/translate" {
        return handle_papago_image_text(&state, &headers, &content_type, &body).await;
    }
    if path == "/image-to-image/v1/translate" {
        return handle_papago_image_image(&state, &headers, &content_type, &body).await;
    }
    if path == "/doc-trans/v1/translate" {
        return handle_papago_doc(&state, &headers).await;
    }
    if path == "/v2/translation/translate" {
        return handle_kakao(&state, &headers, &body_text).await;
    }
    if path == "/translate/v2/translate" {
        return handle_yandex(&state, &headers, &body_text).await;
    }
    if path.starts_with("/v3/projects/") && path.ends_with(":translateText") {
        return handle_google_text(&state, &headers, &body_text).await;
    }
    if path.starts_with("/v3/projects/") && path.ends_with(":translateDocument") {
        return handle_google_document(&state, &headers).await;
    }
    if path == "/language/translate/v2" {
        return handle_google_v2_like(&state, &headers, &query, &body_text).await;
    }
    if path == "/v1/images:annotate" {
        return handle_google_vision(&state, &headers).await;
    }
    if path == "/translate" && query.get("api-version").map(String::as_str) == Some("3.0") {
        return handle_azure_text(&state, &headers, &query, &body_text).await;
    }
    if path == "/translate" && header_value(&headers, "ocp-apim-subscription-key").is_some() {
        return handle_azure_text(&state, &headers, &query, &body_text).await;
    }
    if path.starts_with("/translator/text/batch/v1.0/batches") {
        return handle_azure_batch_document(&state, &headers, &method, &path).await;
    }
    if path.ends_with("/translator/document:translate") {
        return handle_azure_document(&state, &headers).await;
    }
    if path.ends_with("/computervision/imageanalysis:analyze") {
        return handle_azure_vision(&state, &headers).await;
    }
    if path.contains("/videotranslation/translations/") {
        return handle_azure_video(&state, &headers, &method, &path).await;
    }
    if path == "/api/trans/sdk/picture" {
        return handle_baidu_sdk_picture(&state, &body_text).await;
    }
    if path == "/api/translate/image" {
        return handle_alibaba_image(&state, &headers, &body_text).await;
    }
    if path == "/rpc/2.0/mt/texttrans/v1" || path == "/rpc/2.0/mt/texttrans-with-dict/v1" {
        return handle_baidu_cloud_text(&state, &headers, &body_text).await;
    }
    if path == "/file/2.0/mt/pictrans/v1" {
        return handle_baidu_cloud_image(&state, &headers).await;
    }
    if path == "/rpc/2.0/mt/v2/speech-translation" {
        return handle_baidu_cloud_speech(&state, &headers, &body_text).await;
    }
    if path == "/rpc/2.0/mt/v2/doc-translation/create" {
        return handle_baidu_cloud_doc_submit(&state, &headers).await;
    }
    if path == "/rpc/2.0/mt/v2/doc-translation/query" {
        return handle_baidu_cloud_doc_query(&state, &headers, &body_text).await;
    }
    if path == "/ocrtransapi" {
        return handle_youdao_image(&state, &body_text).await;
    }
    if path == "/api" {
        return handle_youdao_text(&state, &body_text).await;
    }
    if path == "/speechtransapi" {
        return handle_youdao_speech(&state, &body_text).await;
    }
    if path == "/file_trans/upload" {
        return handle_youdao_doc(&state, &body_text).await;
    }
    if path == "/translate_html" {
        return handle_youdao_html(&state, &body_text).await;
    }
    if path == "/translate" && query.get("langpair").is_some() {
        return handle_apertium_text(&state, &query).await;
    }
    if path == "/apy/translate" && query.get("langpair").is_some() {
        return handle_apertium_text(&state, &query).await;
    }
    if path == "/translateDoc" || path == "/apy/translateDoc" {
        return handle_apertium_doc(&state).await;
    }
    if path == "/translate" && body_text.contains("\"source_lang\"") && body_text.contains("\"target_lang\"") {
        return handle_deeplx(&state, &headers, &body_text).await;
    }
    if path == "/translate" && headers.contains_key("mmt-apikey") {
        return handle_modernmt(&state, &headers, &body_text).await;
    }
    if path == "/translate"
        && header_value(&headers, "authorization")
            .map(|v| v.trim().to_ascii_lowercase().starts_with("bearer "))
            .unwrap_or(false)
        && (body_text.contains("\"q\"") || body_text.contains("\"text\""))
    {
        return handle_modernmt(&state, &headers, &body_text).await;
    }
    if path.contains("/client/v4/accounts/") && path.contains("/ai/run/@cf/meta/") {
        return handle_cloudflare(&state, &headers, &body_text).await;
    }
    if path.contains("/hf-inference/models/") {
        return handle_huggingface_router(&state, &headers, &body_text).await;
    }
    if path == "/NiuTransServer/translation" {
        return handle_niutrans_text(&state, &headers, &body_text).await;
    }
    if path == "/api/translate/web" {
        return handle_alibaba_signed(&state, &body_text).await;
    }
    if path == "/v1/its" || path == "/v2/its" {
        return handle_iflytek_text(&state, &headers, &body_text).await;
    }
    if path == "/translate/v1/translation" {
        return handle_reverso_text(&state, &headers, &body_text).await;
    }
    if path == "/v1/translation" {
        return handle_unbabel_text(&state, &headers, &body_text).await;
    }
    if path == "/v1/translate" {
        return handle_cloudtranslation_text(&state, &headers, &body_text).await;
    }
    if path == "/v2/files" {
        return handle_lilt_file_upload(&state).await;
    }
    if path == "/v2/translate/file" && method == Method::POST {
        return handle_lilt_file_submit(&state).await;
    }
    if path == "/v2/translate/file" && method == Method::GET {
        return handle_lilt_file_poll(&state, &query).await;
    }
    if path == "/v2/translate/files" {
        return handle_lilt_file_download(&state, &query).await;
    }
    if path == "/v2/translate/html" {
        return handle_translateplus_html(&state, &headers, &body_text).await;
    }
    if path == "/v2/translate/email" {
        return handle_translateplus_email(&state, &headers, &body_text).await;
    }
    if path == "/v2/translate/subtitles" {
        return handle_translateplus_subtitles(&state, &headers, &body_text).await;
    }
    if path == "/v2/translate/i18n" {
        return handle_translateplus_i18n_submit(&state, &headers).await;
    }
    if path.starts_with("/v2/translate/i18n/jobs/") {
        return handle_translateplus_i18n_poll_or_download(&state, &path).await;
    }
    if path == "/api/translate/text" {
        return handle_tilde_text(&state, &headers, &body_text).await;
    }
    if path == "/api/translate/file" {
        return handle_tilde_file_submit(&state, &headers, &content_type, &body).await;
    }
    if path.starts_with("/api/translate/file/") {
        return handle_tilde_file_poll_or_download(&state, &path).await;
    }
    if path == "/v2/translate/text" {
        return handle_doctranslate_text_submit(&state, &headers, &content_type, &body).await;
    }
    if path.starts_with("/v1/result/") {
        return handle_doctranslate_v1_result(&state, &path).await;
    }
    if path == "/v3/translate" {
        return handle_doctranslate_v3_submit(&state, &headers, &content_type, &body).await;
    }
    if path.starts_with("/v3/result/") {
        return handle_doctranslate_v3_result(&state, &path).await;
    }
    if path == "/v1/upload" {
        return handle_doctranslate_video_upload(&state).await;
    }
    if path == "/v2/process" {
        return handle_doctranslate_video_process(&state, &body_text).await;
    }
    if path == "/v2/doc/translate/upload" {
        return handle_niutrans_upload(&state, "document").await;
    }
    if path == "/v2/image/translate/upload" {
        return handle_niutrans_upload(&state, "image").await;
    }
    if path.starts_with("/v2/doc/translate/status/") {
        return handle_niutrans_status(&state, &path).await;
    }
    if path.starts_with("/v2/image/translate/status/") {
        return handle_niutrans_status(&state, &path).await;
    }
    if path.starts_with("/v2/doc/translate/download/") {
        return handle_niutrans_download(&state, &path, "document").await;
    }
    if path.starts_with("/v2/image/translate/download/") {
        return handle_niutrans_download(&state, &path, "image").await;
    }
    if path == "/nlp-v2/translate/language/en" || path.starts_with("/nlp-v2/translate/language/") {
        return handle_cloudmersive_text(&state, &headers, &body_text).await;
    }
    if path.starts_with("/convert/translate/document/docx/to/") {
        return handle_cloudmersive_document().await;
    }
    if path.ends_with("/machine-translation/text-translation") {
        return handle_huawei_text(&state, &headers, &body_text).await;
    }
    if path.ends_with("/machine-translation/file-translation") {
        return handle_huawei_doc_submit(&state, &headers).await;
    }
    if path.contains("/machine-translation/file-translation/") {
        return handle_huawei_doc_poll(&state, &path).await;
    }
    if path == "/translation/text/translate" {
        return handle_systran_text(&state, &headers, &body_text).await;
    }
    if path == "/translation/file/translate" {
        return handle_systran_file_submit(&state, &headers, &content_type, &body).await;
    }
    if path == "/translation/file/status" {
        return handle_systran_file_status(&state, &query).await;
    }
    if path == "/translation/file/result" {
        return handle_systran_file_result(&state, &query).await;
    }
    if path == "/v2/assets" {
        return handle_synthesia_asset_prepare(&state, &headers).await;
    }
    if path == "/v2/dubbing" {
        return handle_synthesia_dubbing_submit(&state, &headers).await;
    }
    if path.starts_with("/v2/dubbing/") {
        return handle_synthesia_dubbing_poll(&state, &path).await;
    }
    if path == "/v1/dubbing" {
        return handle_elevenlabs_dubbing_submit(&state, &headers).await;
    }
    if path.starts_with("/v1/dubbing/") && path.contains("/audio/") {
        return handle_elevenlabs_dubbing_download(&state, &path).await;
    }
    if path.starts_with("/v1/dubbing/") {
        return handle_elevenlabs_dubbing_poll(&state, &path).await;
    }
    if path == "/v2/video_translate" {
        return handle_heygen_submit(&state, &headers).await;
    }
    if (path.starts_with("/v1/video_translate/") && path.ends_with("/status")) || path.starts_with("/v2/video_translate/") {
        return handle_heygen_poll(&state, &path).await;
    }
    if path == "/api/library/v1/media/link" {
        return handle_rask_media_prepare(&state, &headers).await;
    }
    if path == "/v2/projects" {
        return handle_rask_project_submit(&state, &headers, &body_text).await;
    }
    if path.starts_with("/v2/projects/") {
        return handle_rask_project_poll(&state, &path).await;
    }
    if path == "/api/v1/automation/get-upload-url" {
        return handle_reap_prepare(&state, &headers).await;
    }
    if path == "/api/v1/automation/create-dubbing" {
        return handle_reap_submit(&state, &headers).await;
    }
    if path == "/api/v1/automation/get-project-status" {
        return handle_reap_status(&state, &query).await;
    }
    if path == "/api/v1/automation/get-project-clips" {
        return handle_reap_clips(&state, &query).await;
    }
    if method == Method::PUT && path.starts_with("/mock-upload/") {
        return StatusCode::OK.into_response();
    }
    if path.starts_with("/instances/") && path.contains("/v3/documents/") && path.contains("/translated_document") {
        return handle_ibm_document_download(&state, &path).await;
    }
    if path.starts_with("/instances/") && path.contains("/v3/documents/") {
        return handle_ibm_document_poll(&state, &path).await;
    }
    if path.starts_with("/instances/") && path.contains("/v3/documents") {
        return handle_ibm_document_submit(&state, &headers).await;
    }
    if path.starts_with("/instances/") && path.contains("/v3/translate") {
        return handle_ibm_text(&state, &headers, &body_text).await;
    }
    if path == "/v1/translator" {
        return handle_caiyun_text(&state, &headers, &body_text).await;
    }
    if path == "/b1/api/v3/translate" {
        return handle_lingvanex_text(&state, &headers, &body_text).await;
    }
    if path == "/get" && query.get("q").is_some() {
        return handle_mymemory_text(&state, &query).await;
    }
    if path == "/v3/universal-ai" {
        return handle_eden_ai(&state, &headers, &body_text).await;
    }
    if path == "/translations" {
        return handle_did_submit(&state, &headers, &body_text).await;
    }
    if path.starts_with("/translations/") {
        return handle_did_poll(&state, &path).await;
    }
    if path.starts_with("/model/") && path.ends_with("/converse") {
        return handle_bedrock_converse(&state, &headers, &body_text).await;
    }
    if path == "/api/v1/services/aigc/multimodal-generation/generation" {
        return handle_dashscope_qwen_image_submit(&state, &headers, &body_text).await;
    }
    if path.starts_with("/api/v1/tasks/") {
        return handle_dashscope_qwen_image_poll(&state, &path).await;
    }
    if path == "/" && headers.contains_key("x-tc-action") {
        return handle_tencent_signed(&state, &headers, &body_text).await;
    }
    if path == "/" && headers.contains_key("x-amz-target") {
        return handle_aws_signed(&state, &headers, &body_text).await;
    }
    if path == "/" && query.get("Action").is_some() && headers.contains_key("authorization") {
        return handle_volcengine_official(&state, &method, &headers, &query, &body_text).await;
    }
    if path == "/" && body_text.contains("AccessKeyId=") && body_text.contains("Signature=") {
        return handle_alibaba_signed(&state, &body_text).await;
    }
    if path == "/" && headers.contains_key("rev-api-key") {
        return handle_reverie(&state, &headers, &body_text).await;
    }
    if path.starts_with("/NexRelay/v1/translate") {
        return handle_pangeanic_text(&state, &headers, &body_text).await;
    }
    if path == "/PGFile/v1/sendfile" {
        return handle_pangeanic_doc_submit(&state).await;
    }
    if path == "/PGFile/v1/checkfile" {
        return handle_pangeanic_doc_poll(&state, &query).await;
    }
    if path == "/PGFile/v1/download" {
        return handle_pangeanic_doc_download(&state, &query).await;
    }
    if path == "/v1/transcription_or_translation" {
        return handle_reka(&state, &headers, &body_text).await;
    }
    if path.ends_with("/translation") {
        return handle_nlpcloud(&state, &headers, &body_text).await;
    }
    if path == "/dmp/translate/v1" {
        return handle_fptai(&state, &headers, &body_text).await;
    }
    if path == "/api/trans/vip/translate" {
        return handle_baidu_open_text(&state, &body_text).await;
    }
    if path == "/translate" && body_text.contains("\"q\"") && body_text.contains("\"target\"") {
        return handle_libretranslate_text(&state, &headers, &body_text).await;
    }
    if path == "/translate" && query.get("api-version").is_none() && body_text.contains("\"text\"") {
        return handle_generic_text_json(&state, &headers, &body_text).await;
    }
    if path == "/v1/embed" {
        return handle_cohere_embed(&state, &headers, &body_text).await;
    }
    if path == "/v1/rerank" {
        return handle_cohere_rerank(&state, &headers, &body_text).await;
    }
    if path.starts_with("/v1/text-to-speech/") {
        return handle_elevenlabs_tts(&state, &headers).await;
    }
    if path == "/v1/moderations" {
        return handle_openai_moderation(&state, &headers, &body_text).await;
    }
    if path == "/v2/transcript" {
        return handle_assemblyai_sentiment(&state, &headers).await;
    }
    if path.starts_with("/v1/listen") {
        return handle_deepgram_subtitle(&state, &headers, &body_text).await;
    }
    if path == "/v1/scrape" || path == "/v1/crawl" {
        return handle_firecrawl(&state, &headers, &path, &body_text).await;
    }
    if path.contains("/indexes/") && path.ends_with("/search") {
        return handle_meilisearch_search(&state, &headers, &body_text).await;
    }
    if path.contains("/workflows/") && path.ends_with("/execute") {
        return handle_n8n_workflow(&state, &headers).await;
    }
    // ntfy-style publish: POST /{topic}
    if method == Method::POST
        && path.starts_with('/')
        && path.matches('/').count() == 1
        && path.len() > 1
        && !path.starts_with("/api")
        && !path.starts_with("/v1")
        && !path.starts_with("/v2")
    {
        return handle_ntfy_publish(&state, &headers, &path).await;
    }

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "official_mock_not_implemented",
            "path": path,
            "method": method.as_str()
        })),
    )
        .into_response()
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

fn parse_query(query: Option<&str>) -> HashMap<String, String> {
    query
        .map(|query| {
            form_urlencoded::parse(query.as_bytes())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_form(body: &str) -> HashMap<String, String> {
    form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn parse_json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| json!({}))
}

fn parse_multipart(content_type: &str, body: &[u8]) -> MultipartPayload {
    let Some(boundary) = content_type
        .split(';')
        .map(|segment| segment.trim())
        .find_map(|segment| segment.strip_prefix("boundary=").map(str::to_string))
    else {
        return MultipartPayload::default();
    };

    let boundary_marker = format!("--{}", boundary.trim_matches('"'));
    let body_text = String::from_utf8_lossy(body);
    let mut payload = MultipartPayload::default();

    for part in body_text.split(&boundary_marker) {
        let part = part.trim();
        if part.is_empty() || part == "--" {
            continue;
        }
        let Some((header_block, content_block)) = part.split_once("\r\n\r\n") else {
            continue;
        };
        let content = content_block
            .trim_end_matches("\r\n")
            .trim_end_matches("--")
            .to_string();
        let Some(disposition) = header_block
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("content-disposition:"))
        else {
            continue;
        };

        let mut field_name = None;
        let mut file_name = None;
        for segment in disposition.split(';') {
            let segment = segment.trim();
            if let Some(value) = segment.strip_prefix("name=") {
                field_name = Some(value.trim_matches('"').to_string());
            }
            if let Some(value) = segment.strip_prefix("filename=") {
                file_name = Some(value.trim_matches('"').to_string());
            }
        }

        if let Some(name) = field_name {
            if let Some(filename) = file_name {
                payload.filenames.insert(name, filename);
            } else {
                payload.fields.insert(name, content);
            }
        }
    }

    payload
}

fn build_text(source: &str, target_lang: &str) -> String {
    let text = if source.trim().is_empty() {
        "mock translated content"
    } else {
        source
    };
    translate_text(text, target_lang)
}

fn base64_of(kind: &str) -> String {
    let bytes = translated_file_bytes(kind);
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD.encode(bytes)
}

fn translated_file_bytes(kind: &str) -> Vec<u8> {
    let path = match kind {
        "image" => "media/translated/image-translated.png",
        "audio" => "media/translated/audio-translated.mp3",
        "video" => "media/translated/video-translated.mp4",
        _ => "media/translated/document-translated.pdf",
    };
    fs::read(path).unwrap_or_else(|_| format!("mock-{kind}-binary").into_bytes())
}

fn translated_binary_response(kind: &str, filename: Option<&str>, content_type: Option<&str>) -> Response {
    let bytes = translated_file_bytes(kind);
    let default_content_type = match kind {
        "image" => "image/png",
        "audio" => "audio/mpeg",
        "video" => "video/mp4",
        _ => "application/octet-stream",
    };
    let mut builder = Response::builder()
        .status(200)
        .header("Content-Type", content_type.unwrap_or(default_content_type))
        .header("Content-Length", bytes.len().to_string());
    if let Some(filename) = filename {
        builder = builder.header(
            "Content-Disposition",
            format!("attachment; filename=\"{filename}\""),
        );
    }
    builder.body(axum::body::Body::from(bytes)).unwrap()
}

fn next_job_id(state: &AppState, prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let seq = state.total_requests.load(Ordering::Relaxed);
    format!("{prefix}-{millis}-{seq}")
}

async fn store_job(
    state: &AppState,
    job_id: &str,
    family: &str,
    poll_response: Value,
    download_kind: Option<&str>,
    download_filename: Option<String>,
    download_content_type: Option<&str>,
) {
    let mut jobs = state.official_jobs.lock().await;
    jobs.insert(
        job_id.to_string(),
        OfficialJob {
            family: family.to_string(),
            poll_response,
            result_response: None,
            download_kind: download_kind.map(str::to_string),
            download_filename,
            download_content_type: download_content_type.map(str::to_string),
            poll_count: 0,
        },
    );
}

async fn load_job(state: &AppState, job_id: &str) -> Option<OfficialJob> {
    state.official_jobs.lock().await.get(job_id).cloned()
}

fn json_response(value: Value) -> Response {
    let mut resp = Json(value.clone()).into_response();
    if let Some(err_code) = value.get("error_code").and_then(Value::as_str) {
        if let Ok(val) = HeaderValue::from_str(&format!("baidu_error_{}", err_code)) {
            resp.headers_mut().insert("x-mock-auth-error", val);
        }
        resp.headers_mut().insert("x-mock-vendor", HeaderValue::from_static("baidu"));
    } else if let Some(err_code) = value.get("errorCode").and_then(Value::as_str) {
        if err_code != "0" {
            if let Ok(val) = HeaderValue::from_str(&format!("youdao_error_{}", err_code)) {
                resp.headers_mut().insert("x-mock-auth-error", val);
            }
        }
        resp.headers_mut().insert("x-mock-vendor", HeaderValue::from_static("youdao"));
    } else if let Some(err) = value.pointer("/Response/Error/Code").and_then(Value::as_str) {
        if let Ok(val) = HeaderValue::from_str(&format!("tencent_{}", err)) {
            resp.headers_mut().insert("x-mock-auth-error", val);
        }
        resp.headers_mut().insert("x-mock-vendor", HeaderValue::from_static("tencent"));
    }
    resp
}

fn text_from_openai_like(value: &Value) -> String {
    value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().rev().find_map(|message| {
                if message.get("role").and_then(Value::as_str) != Some("user") {
                    return None;
                }
                if let Some(text) = message.get("content").and_then(Value::as_str) {
                    return Some(text.to_string());
                }
                message
                    .get("content")
                    .and_then(Value::as_array)
                    .and_then(|parts| {
                        parts.iter().find_map(|part| {
                            part.get("text")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                    })
            })
        })
        .unwrap_or_default()
}

fn target_lang_from_openai_like(value: &Value) -> String {
    if let Some(target) = value
        .get("translation_options")
        .and_then(|opts| opts.get("target_lang"))
        .and_then(Value::as_str)
    {
        return target.to_string();
    }
    let prompt = value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                if message.get("role").and_then(Value::as_str) == Some("system") {
                    message.get("content").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                }
            })
        })
        .or_else(|| value.get("system").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    extract_target_lang(&prompt).unwrap_or_else(|| "en".to_string())
}

fn extract_target_lang(prompt: &str) -> Option<String> {
    let lower = prompt.to_ascii_lowercase();
    let index = lower.find(" to ")?;
    let tail = &prompt[index + 4..];
    let candidate = tail
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'));
    if candidate.is_empty() {
        None
    } else {
        Some(candidate.to_string())
    }
}

async fn handle_openai_like(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let key_id = header_value(headers, "authorization").unwrap_or_else(|| "official:none".into());
    let active = key_enter(state, &format!("official:chat:{key_id}")).await;
    if let Err(error) = verify::verify_bearer_api_key(&key_id) {
        key_exit(state, &format!("official:chat:{key_id}")).await;
        return auth_error_response(error);
    }
    let value = parse_json(body);
    let translated = build_text(&text_from_openai_like(&value), &target_lang_from_openai_like(&value));
    let response = json_response(json!({
        "id": format!("chatcmpl-official-{active}"),
        "object": "chat.completion",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": translated } }]
    }));
    key_exit(state, &format!("official:chat:{key_id}")).await;
    response
}

async fn handle_openai_audio(state: &AppState, headers: &HeaderMap, content_type: &str, body: &[u8]) -> Response {
    let auth = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:openai-audio:{}", if auth.is_empty() { "none" } else { &auth });
    let _ = key_enter(state, &stat_key).await;

    if auth.is_empty() || auth.contains("bad") || auth.contains("invalid") || verify::verify_bearer_api_key(&auth).is_err() {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "You didn't provide an API key. You need to provide your API key in an Authorization header using Bearer format (like Authorization: Bearer YOUR_KEY).",
                    "type": "invalid_request_error",
                    "param": null,
                    "code": null
                }
            })),
        )
            .into_response();
    }

    let payload = parse_multipart(content_type, body);
    let prompt = payload.fields.get("prompt").map(String::as_str).unwrap_or("audio transcription");
    let response_format = payload.fields.get("response_format").map(String::as_str).unwrap_or("json");
    let translated = build_text(prompt, "en");
    key_exit(state, &stat_key).await;

    match response_format {
        "text" => translated.into_response(),
        "srt" => format!("1\n00:00:00,000 --> 00:00:05,000\n{}\n", translated).into_response(),
        "vtt" => format!("WEBVTT\n\n1\n00:00:00.000 --> 00:00:05.000\n{}\n", translated).into_response(),
        "verbose_json" => json_response(json!({
            "task": "translate",
            "language": "english",
            "duration": 5.0,
            "text": translated,
            "segments": [{
                "id": 0,
                "seek": 0,
                "start": 0.0,
                "end": 5.0,
                "text": translated
            }]
        })),
        _ => json_response(json!({ "text": translated })),
    }
}

async fn handle_anthropic(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let key = header_value(headers, "x-api-key").unwrap_or_default();
    let stat_key = format!("official:anthropic:{}", if key.is_empty() { "none" } else { &key });
    let _ = key_enter(state, &stat_key).await;

    if key.is_empty() || key.contains("bad") || key.contains("invalid") || (key != crate::config::BEARER_KEY && key != "mock-anthropic-key") {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "type": "error",
                "error": {
                    "type": "authentication_error",
                    "message": "invalid x-api-key"
                }
            })),
        )
            .into_response();
    }

    let value = parse_json(body);
    let text = text_from_openai_like(&value);
    let target = target_lang_from_openai_like(&value);
    let translated = build_text(&text, &target);
    let model = value.get("model").and_then(Value::as_str).unwrap_or("claude-3-5-sonnet-20241022");
    let response = json_response(json!({
        "id": format!("msg_{}", next_job_id(state, "anthropic")),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{ "type": "text", "text": translated }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 10,
            "output_tokens": 15
        }
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_cohere(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let key = header_value(headers, "authorization").unwrap_or_else(|| "none".into());
    let stat_key = format!("official:cohere:{key}");
    let _ = key_enter(state, &stat_key).await;
    let value = parse_json(body);
    let translated = build_text(&text_from_openai_like(&value), &target_lang_from_openai_like(&value));
    let response = json_response(json!({ "message": { "content": [{ "text": translated }] } }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_cohere_embed(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "cohere-embed", headers).await;
    let value = parse_json(body);
    let count = value
        .get("texts")
        .and_then(Value::as_array)
        .map(|items| items.len().max(1))
        .unwrap_or(1);
    let embeddings = (0..count)
        .map(|idx| json!([0.01 * (idx as f64 + 1.0), 0.02, 0.03, 0.04]))
        .collect::<Vec<_>>();
    json_response(json!({ "embeddings": embeddings, "id": "mock-embed" }))
}

async fn handle_cohere_rerank(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "cohere-rerank", headers).await;
    let value = parse_json(body);
    let docs = value
        .get("documents")
        .cloned()
        .unwrap_or_else(|| json!(["doc-a", "doc-b"]));
    let results = match docs {
        Value::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(idx, doc)| {
                json!({
                    "index": idx,
                    "relevance_score": 1.0 - (idx as f64) * 0.1,
                    "document": doc
                })
            })
            .collect::<Vec<_>>(),
        other => vec![json!({ "index": 0, "relevance_score": 1.0, "document": other })],
    };
    json_response(json!({ "results": results, "id": "mock-rerank" }))
}

async fn handle_elevenlabs_tts(state: &AppState, headers: &HeaderMap) -> Response {
    let api_key = header_value(headers, "xi-api-key").unwrap_or_default();
    let stat_key = format!("official:elevenlabs-tts:{}", if api_key.is_empty() { "none" } else { &api_key });
    let _ = key_enter(state, &stat_key).await;

    if api_key.is_empty() || api_key.contains("bad") || api_key.contains("invalid") || (api_key != crate::config::BEARER_KEY && api_key != "mock-elevenlabs-key") {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "detail": {
                    "status": "invalid_api_key",
                    "message": "Invalid API key provided."
                }
            })),
        )
            .into_response();
    }
    key_exit(state, &stat_key).await;
    let accept = header_value(headers, "accept").unwrap_or_default().to_ascii_lowercase();
    if accept.contains("application/json") {
        json_response(json!({
            "audio_url": translated_media_url("audio")
        }))
    } else {
        translated_binary_response("audio", Some("speech.mp3"), Some("audio/mpeg"))
    }
}

async fn handle_openai_moderation(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "openai-moderation", headers).await;
    let _ = parse_json(body);
    json_response(json!({
        "results": [{
            "flagged": false,
            "categories": { "hate": false, "violence": false },
            "category_scores": { "hate": 0.01, "violence": 0.02 }
        }]
    }))
}

async fn handle_assemblyai_sentiment(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "assemblyai-sentiment", headers).await;
    let job_id = next_job_id(state, "assemblyai-sentiment");
    json_response(json!({
        "id": job_id,
        "status": "completed",
        "sentiment_analysis_results": [
            { "text": "mock positive", "sentiment": "POSITIVE", "confidence": 0.91 }
        ]
    }))
}

async fn handle_deepgram_subtitle(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "deepgram-subtitle", headers).await;
    let value = parse_json(body);
    let source = value
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("subtitle source");
    let transcript = build_text(source, "en");
    json_response(json!({
        "results": {
            "channels": [{
                "alternatives": [{
                    "transcript": transcript,
                    "words": [{ "word": "mock", "start": 0.0, "end": 0.4 }]
                }]
            }]
        }
    }))
}

async fn handle_firecrawl(state: &AppState, headers: &HeaderMap, path: &str, body: &str) -> Response {
    simple_stat(state, "firecrawl", headers).await;
    let value = parse_json(body);
    let url = value
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("https://example.com");
    if path.ends_with("/crawl") {
        return json_response(json!({
            "success": true,
            "id": next_job_id(state, "firecrawl-crawl"),
            "url": url
        }));
    }
    json_response(json!({
        "success": true,
        "data": {
            "markdown": format!("# mock crawl\n\n{}", build_text(url, "en")),
            "html": format!("<p>{}</p>", build_text(url, "en")),
            "metadata": { "sourceURL": url }
        }
    }))
}

async fn handle_meilisearch_search(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "meilisearch-search", headers).await;
    let value = parse_json(body);
    let query = value.get("q").and_then(Value::as_str).unwrap_or("query");
    json_response(json!({
        "hits": [
            { "id": 1, "title": build_text(query, "en"), "content": "mock hit" }
        ],
        "query": query,
        "processingTimeMs": 1
    }))
}

async fn handle_n8n_workflow(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "n8n-workflow", headers).await;
    json_response(json!({
        "executionId": next_job_id(state, "n8n"),
        "finished": true
    }))
}

async fn handle_ntfy_publish(state: &AppState, headers: &HeaderMap, path: &str) -> Response {
    simple_stat(state, "ntfy-publish", headers).await;
    let topic = path.trim_start_matches('/');
    json_response(json!({
        "id": next_job_id(state, "ntfy"),
        "topic": topic,
        "time": 1_700_000_000
    }))
}

async fn handle_gemini(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let stat_key = format!("official:gemini:{}", header_value(headers, "authorization").unwrap_or_else(|| "none".into()));
    let _ = key_enter(state, &stat_key).await;
    let value = parse_json(body);
    let text = value
        .get("contents")
        .and_then(Value::as_array)
        .and_then(|contents| contents.first())
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let translated = build_text(text, extract_target_lang(text).as_deref().unwrap_or("en"));
    let response = json_response(json!({
        "candidates": [{ "content": { "parts": [{ "text": translated }] } }]
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_deepl_text(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    content_type: &str,
    body: &str,
) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    // Official auth channels: `DeepL-Auth-Key` header, `?auth_key=` query
    // param, or `auth_key` form field (legacy clients). All resolve to the
    // same key material for verification.
    let form_pairs: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let form_auth_key = form_pairs
        .iter()
        .find(|(key, _)| key == "auth_key")
        .map(|(_, value)| value.clone());
    let credential = if !authorization.is_empty() {
        authorization
    } else if let Some(key) = query.get("auth_key") {
        key.clone()
    } else {
        form_auth_key.unwrap_or_default()
    };
    let stat_key = format!(
        "official:deepl:{}",
        if credential.is_empty() {
            "none".into()
        } else {
            credential.clone()
        }
    );
    let _ = key_enter(state, &stat_key).await;
    if let Err(_error) = verify::verify_deepl_auth_key(&credential) {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "message": "Authorization failed, please check your DeepL-Auth-Key"
            })),
        )
            .into_response();
    }
    // Input: JSON body (`text` as single string or array of strings) or
    // form-urlencoded body with one or more `text=` entries.
    let is_form = content_type
        .to_ascii_lowercase()
        .contains("application/x-www-form-urlencoded");
    let mut texts: Vec<String> = Vec::new();
    let mut target = "EN".to_string();
    let mut source: Option<String> = None;
    if is_form {
        for (key, value) in &form_pairs {
            match key.as_str() {
                "text" => texts.push(value.clone()),
                "target_lang" => target = value.clone(),
                "source_lang" => source = Some(value.clone()),
                _ => {}
            }
        }
    } else {
        let value = parse_json(body);
        match value.get("text") {
            Some(Value::Array(items)) => {
                for item in items {
                    if let Some(text) = item.as_str() {
                        texts.push(text.to_string());
                    }
                }
            }
            Some(Value::String(text)) => texts.push(text.clone()),
            _ => {}
        }
        if let Some(lang) = value.get("target_lang").and_then(Value::as_str) {
            target = lang.to_string();
        }
        if let Some(lang) = value.get("source_lang").and_then(Value::as_str) {
            source = Some(lang.to_string());
        }
    }

    if !crate::languages::is_valid_deepl_target(&target) {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "Value for 'target_lang' not supported."
            })),
        )
            .into_response();
    }
    if let Some(ref s) = source {
        if !crate::languages::is_valid_deepl_source(s) {
            key_exit(state, &stat_key).await;
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "Value for 'source_lang' not supported."
                })),
            )
                .into_response();
        }
    }

    // One `translations[]` entry per input element, in order, with
    // detected_source_language like the official v2 response.
    let translations: Vec<Value> = texts
        .iter()
        .map(|text| {
            json!({
                "detected_source_language": source.clone().unwrap_or_else(|| "EN".to_string()),
                "text": build_text(text, &target),
            })
        })
        .collect();
    let response = json_response(json!({ "translations": translations }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_deepl_submit(state: &AppState, headers: &HeaderMap, content_type: &str, body: &[u8]) -> Response {
    let auth = header_value(headers, "authorization").unwrap_or_default();
    if verify::verify_deepl_auth_key(&auth).is_err() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "message": "Authorization failed, please check your DeepL-Auth-Key"
            })),
        )
            .into_response();
    }
    let payload = parse_multipart(content_type, body);
    let target = payload.fields.get("target_lang").cloned().unwrap_or_else(|| "EN".into());
    if !crate::languages::is_valid_deepl_target(&target) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "Value for 'target_lang' not supported."
            })),
        )
            .into_response();
    }
    let file_name = payload
        .filenames
        .get("file")
        .cloned()
        .unwrap_or_else(|| "source.bin".to_string());
    let media_kind = if file_name.ends_with(".png") || file_name.ends_with(".jpg") || file_name.ends_with(".jpeg") {
        "image"
    } else {
        "document"
    };
    let job_id = next_job_id(state, "deepl");
    let document_key = format!("mock-doc-key-{}", job_id);
    store_job(
        state,
        &job_id,
        "deepl",
        json!({ "document_id": job_id, "status": "translating", "seconds_remaining": 5 }),
        Some(media_kind),
        Some(file_name),
        Some("application/octet-stream"),
    )
    .await;
    let response = json_response(json!({ "document_id": job_id, "document_key": document_key }));
    let key = format!("official:deepl-doc:{}", if auth.is_empty() { "none" } else { &auth });
    let _ = key_enter(state, &key).await;
    key_exit(state, &key).await;
    response
}

async fn handle_deepl_poll_or_result(state: &AppState, _headers: &HeaderMap, path: &str) -> Response {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() < 3 {
        return (StatusCode::BAD_REQUEST, Json(json!({ "message": "invalid deepl path" }))).into_response();
    }
    let job_id = parts[2];
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "job not found" }))).into_response();
    };
    if parts.len() >= 4 && parts[3] == "result" {
        return translated_binary_response(
            job.download_kind.as_deref().unwrap_or("document"),
            job.download_filename.as_deref(),
            job.download_content_type.as_deref(),
        );
    }
    let count = job.poll_count;
    let mut updated_job = job.clone();
    updated_job.poll_count += 1;
    state.official_jobs.lock().await.insert(job_id.to_string(), updated_job);

    if count == 0 {
        json_response(json!({
            "document_id": job_id,
            "status": "translating",
            "seconds_remaining": 5
        }))
    } else {
        json_response(json!({
            "document_id": job_id,
            "status": "done",
            "billed_characters": 100
        }))
    }
}

async fn handle_papago_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let client_id = header_value(headers, "x-naver-client-id").unwrap_or_default();
    let client_secret = header_value(headers, "x-naver-client-secret").unwrap_or_default();
    let stat_key = format!("official:papago:{client_id}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_papago(&client_id, &client_secret) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let form = parse_form(body);
    let translated = build_text(form.get("text").map(String::as_str).unwrap_or_default(), form.get("target").map(String::as_str).unwrap_or("en"));
    key_exit(state, &stat_key).await;
    json_response(json!({ "message": { "result": { "translatedText": translated } } }))
}

async fn handle_papago_webpage(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let form = parse_form(body);
    let translated = build_text(form.get("html").map(String::as_str).unwrap_or_default(), form.get("target").map(String::as_str).unwrap_or("en"));
    simple_stat(state, "papago-web", headers).await;
    json_response(json!({ "body": translated }))
}

async fn handle_papago_image_text(state: &AppState, headers: &HeaderMap, content_type: &str, body: &[u8]) -> Response {
    let payload = parse_multipart(content_type, body);
    let target = payload.fields.get("target").map(String::as_str).unwrap_or("en");
    simple_stat(state, "papago-image-text", headers).await;
    json_response(json!({ "data": { "targetText": build_text("image ocr", target) } }))
}

async fn handle_papago_image_image(state: &AppState, headers: &HeaderMap, content_type: &str, body: &[u8]) -> Response {
    let payload = parse_multipart(content_type, body);
    let target = payload.fields.get("target").map(String::as_str).unwrap_or("en");
    let source_ref = payload.fields.get("image").map(String::as_str).unwrap_or("");
    simple_stat(state, "papago-image-image", headers).await;
    json_response(json!({ "translatedImage": translate_media_or_default(source_ref, target, "image") }))
}

async fn handle_papago_doc(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "papago-doc", headers).await;
    json_response(json!({ "data": { "requestId": next_job_id(state, "papago-doc") } }))
}

async fn handle_kakao(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let form = parse_form(body);
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:kakao:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_kakao(&authorization) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let translated = build_text(form.get("query").map(String::as_str).unwrap_or_default(), form.get("target_lang").map(String::as_str).unwrap_or("en"));
    let response = json_response(json!({ "translated_text": [[translated]] }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_yandex(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "yandex", headers).await;
    let value = parse_json(body);
    let translated = build_text(
        value.get("texts").and_then(Value::as_array).and_then(|items| items.first()).and_then(Value::as_str).unwrap_or_default(),
        value.get("targetLanguageCode").and_then(Value::as_str).unwrap_or("en"),
    );
    json_response(json!({ "translations": [{ "text": translated }] }))
}

async fn handle_google_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:google-v3:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    // Prefer OAuth access_token; also accept catalog-style Bearer api_key (BEARER_KEY).
    let auth_ok = verify::verify_google_access_token(&authorization).is_ok()
        || verify::verify_bearer_api_key(&authorization).is_ok();
    if !auth_ok {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "code": 401,
                    "message": "Request had invalid authentication credentials. Expected OAuth 2 access token.",
                    "status": "UNAUTHENTICATED"
                }
            })),
        )
            .into_response();
    }
    let value = parse_json(body);
    let target = value
        .get("targetLanguageCode")
        .or_else(|| value.get("target"))
        .and_then(Value::as_str)
        .unwrap_or("en");
    let source = value
        .get("sourceLanguageCode")
        .or_else(|| value.get("source"))
        .and_then(Value::as_str);
    let is_source_valid = match source {
        None => true,
        Some(s) if s.is_empty() || s == "auto" => true,
        Some(s) => crate::languages::is_valid_google_lang(s),
    };

    if !crate::languages::is_valid_google_lang(target) || !is_source_valid {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "code": 400,
                    "message": "Invalid Value",
                    "status": "INVALID_ARGUMENT",
                    "details": [
                        {
                            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                            "reason": "INVALID_ARGUMENT"
                        }
                    ]
                }
            })),
        )
            .into_response();
    }

    let text = value
        .get("contents")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .or_else(|| value.get("q").and_then(Value::as_str))
        .or_else(|| value.get("text").and_then(Value::as_str))
        .unwrap_or_default();
    if let Some(resp) = crate::auth_profiles::reject_if_too_long(text, 30_000) {
        key_exit(state, &stat_key).await;
        return resp;
    }
    let response = json_response(json!({ "translations": [{ "translatedText": build_text(text, target) }] }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_google_document(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "google-doc", headers).await;
    json_response(json!({ "documentTranslation": { "byteStreamOutputs": [base64_of("document")] } }))
}

async fn handle_google_vision(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "google-vision", headers).await;
    json_response(json!({ "responses": [{ "fullTextAnnotation": { "text": build_text("image text", "en") } }] }))
}

async fn handle_azure_text(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &str,
) -> Response {
    let subscription_key = header_value(headers, "ocp-apim-subscription-key").unwrap_or_default();
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!(
        "official:azure:{}",
        if !subscription_key.is_empty() {
            subscription_key.clone()
        } else {
            authorization.clone()
        }
    );
    let _ = key_enter(state, &stat_key).await;
    // Split auth: subscription key OR OAuth Bearer access_token (Azure Entra).
    let auth_ok = if !subscription_key.is_empty() {
        verify::verify_azure(&subscription_key).is_ok()
    } else {
        verify::verify_azure_access_token(&authorization).is_ok()
    };
    if !auth_ok {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "code": 401000,
                    "message": "The request is not authorized. Check your subscription key or bearer token."
                }
            })),
        )
            .into_response();
    }

    let target = query.get("to").cloned().unwrap_or_else(|| "en".into());
    if !crate::languages::is_valid_azure_target(&target) {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "code": 400036,
                    "message": "The target language is not valid."
                }
            })),
        )
            .into_response();
    }

    let value = parse_json(body);
    // Official v3 body: an array of `{ "Text": "..." }` items (one response
    // element per item). Map EVERY item — never truncate to the first.
    let items: Vec<String> = match &value {
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.get("Text")
                    .or_else(|| item.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect(),
        // Leniency for single-object bodies: treat as a one-item batch.
        Value::Object(_) => vec![value
            .get("Text")
            .or_else(|| value.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()],
        _ => Vec::new(),
    };
    for text in &items {
        if let Some(resp) = crate::auth_profiles::reject_if_too_long(text, 50_000) {
            key_exit(state, &stat_key).await;
            return resp;
        }
    }
    // Official response shape: lowercase `translations[].text`/`to` keys.
    let response = json_response(Value::Array(
        items
            .iter()
            .map(|text| {
                json!({ "translations": [{ "text": build_text(text, &target), "to": target }] })
            })
            .collect(),
    ));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_azure_document(state: &AppState, headers: &HeaderMap) -> Response {
    simple_azure_stat(state, headers).await;
    json_response(json!({ "translatedDocument": translated_media_url("document") }))
}

async fn handle_azure_vision(state: &AppState, headers: &HeaderMap) -> Response {
    simple_azure_stat(state, headers).await;
    json_response(json!({ "readResult": { "blocks": [{ "lines": [{ "text": build_text("ocr text", "en") }] }] } }))
}

async fn handle_azure_video(state: &AppState, headers: &HeaderMap, method: &Method, path: &str) -> Response {
    simple_azure_stat(state, headers).await;
    let job_id = path.rsplit('/').next().unwrap_or("azure-video");
    if *method == Method::PUT {
        let job_id = if job_id.is_empty() { next_job_id(state, "azure-video") } else { job_id.to_string() };
        store_job(
            state,
            &job_id,
            "azure-video",
            json!({
                "id": job_id,
                "status": "Running",
                "progress": 50
            }),
            None,
            None,
            None,
        ).await;
        return json_response(json!({ "id": job_id }));
    }
    let Some(mut job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "job_not_found" }))).into_response();
    };
    let count = job.poll_count;
    job.poll_count += 1;
    state.official_jobs.lock().await.insert(job_id.to_string(), job.clone());

    if count == 0 {
        json_response(json!({
            "id": job_id,
            "status": "Running",
            "progress": 50
        }))
    } else {
        json_response(json!({
            "id": job_id,
            "status": "Succeeded",
            "progress": 100,
            "latestSucceededIteration": {
                "result": {
                    "translatedVideoFileUrl": translated_media_url("video"),
                    "webvttUrl": translated_media_url("document")
                }
            }
        }))
    }
}

async fn handle_azure_batch_document(state: &AppState, headers: &HeaderMap, method: &Method, path: &str) -> Response {
    let key = header_value(headers, "ocp-apim-subscription-key").unwrap_or_default();
    if verify::verify_azure(&key).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "code": "401000",
                    "message": "The request has an invalid API key or is not authorized."
                }
            })),
        ).into_response();
    }

    let job_id = path.rsplit('/').next().unwrap_or("azure-batch-doc");
    if *method == Method::POST {
        let job_id = next_job_id(state, "azure-batch-doc");
        store_job(
            state,
            &job_id,
            "azure-batch-doc",
            json!({
                "id": job_id,
                "status": "Running",
                "summary": { "total": 1, "inProgress": 1, "success": 0 }
            }),
            None,
            None,
            None,
        ).await;

        return Response::builder()
            .status(StatusCode::ACCEPTED)
            .header("Operation-Location", format!("http://127.0.0.1:9090/translator/text/batch/v1.0/batches/{job_id}"))
            .body(axum::body::Body::empty())
            .unwrap_or_default();
    }

    let Some(mut job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "job_not_found" }))).into_response();
    };

    let count = job.poll_count;
    job.poll_count += 1;
    state.official_jobs.lock().await.insert(job_id.to_string(), job.clone());

    if count == 0 {
        json_response(json!({
            "id": job_id,
            "status": "Running",
            "summary": { "total": 1, "inProgress": 1, "success": 0 }
        }))
    } else {
        json_response(json!({
            "id": job_id,
            "status": "Succeeded",
            "summary": { "total": 1, "inProgress": 0, "success": 1 },
            "results": [{
                "sourceUrl": "https://example.com/doc.docx",
                "status": "Succeeded",
                "targetUrl": translated_media_url("document")
            }]
        }))
    }
}

async fn handle_baidu_sdk_picture(state: &AppState, body: &str) -> Response {
    let form = parse_form(body);
    let appid = form.get("appid").cloned().unwrap_or_default();
    let from = form.get("from").cloned().unwrap_or_else(|| "auto".into());
    let to = form.get("to").cloned().unwrap_or_else(|| "zh".into());
    let sign = form.get("sign").cloned().unwrap_or_default();

    let stat_key = format!("official:baidu-pic:{}", &appid);
    let _ = key_enter(state, &stat_key).await;

    if appid.is_empty() || appid != crate::config::BAIDU_APPID {
        key_exit(state, &stat_key).await;
        return json_response(json!({
            "error_code": "52003",
            "error_msg": "UNAUTHORIZED_USER"
        }));
    }

    if sign.is_empty() || sign.contains("bad") || sign.contains("invalid") || sign == "deadbeef" {
        key_exit(state, &stat_key).await;
        return json_response(json!({
            "error_code": "54001",
            "error_msg": "SIGN_ERROR"
        }));
    }

    if !crate::languages::is_valid_baidu_code(&to) || (from != "auto" && !crate::languages::is_valid_baidu_code(&from)) {
        key_exit(state, &stat_key).await;
        return json_response(json!({
            "error_code": "58001",
            "error_msg": "INVALID_TO_PARAM"
        }));
    }

    let translated = build_text("image text", &to);
    let response = json_response(json!({
        "error_code": "0",
        "error_msg": "success",
        "data": {
            "from": from,
            "to": to,
            "pasteImg": translated_media_url("image"),
            "sumDst": translated.clone(),
            "content": [
                {
                    "src": "image text",
                    "dst": translated,
                    "rect": "0 0 100 100",
                    "lineCount": 1
                }
            ]
        }
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_alibaba_image(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let auth = header_value(headers, "authorization").unwrap_or_default();
    let ak = header_value(headers, "x-acs-accesskey-id").unwrap_or_default();
    let form = parse_form(body);
    let json_val = parse_json(body);
    let body_ak = form.get("AccessKeyId").cloned().or_else(|| json_val.get("AccessKeyId").and_then(Value::as_str).map(str::to_string)).unwrap_or_default();

    if auth.contains("bad") || auth.contains("invalid") || ak.contains("bad") || ak.contains("invalid") || body_ak.contains("bad") || body_ak.contains("invalid") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "Code": "InvalidAccessKeyId",
                "Message": "The Access Key ID provided does not exist in our records."
            })),
        ).into_response();
    }

    let target = form.get("TargetLanguage")
        .or_else(|| form.get("target"))
        .cloned()
        .or_else(|| json_val.get("TargetLanguage").and_then(Value::as_str).map(str::to_string))
        .or_else(|| json_val.get("target").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "en".into());
    let translated = build_text("image text", &target);
    json_response(json!({
        "Code": 200,
        "Data": {
            "FinalImageUrl": translated_media_url("image"),
            "InPaintingUrl": translated_media_url("image"),
            "TemplateJson": format!("{{\"lines\":[{{\"text\":\"{}\"}}]}}", translated)
        },
        "RequestId": next_job_id(state, "ali-img")
    }))
}

async fn handle_baidu_cloud_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "baidu-cloud-text", headers).await;
    let value = parse_json(body);
    let translated = build_text(
        value.get("q").and_then(Value::as_str).unwrap_or_default(),
        value.get("to").and_then(Value::as_str).unwrap_or("en"),
    );
    json_response(json!({ "result": { "trans_result": [{ "dst": translated }] } }))
}

async fn handle_baidu_cloud_image(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "baidu-cloud-image", headers).await;
    json_response(json!({ "data": { "sumDst": build_text("image text", "en"), "pasteImg": translated_media_url("image") } }))
}

async fn handle_baidu_cloud_speech(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "baidu-cloud-speech", headers).await;
    let value = parse_json(body);
    let translated = build_text("speech text", value.get("to").and_then(Value::as_str).unwrap_or("en"));
    json_response(json!({ "result": { "target": translated, "target_tts": translated_media_url("audio") } }))
}

async fn handle_baidu_cloud_doc_submit(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "baidu-cloud-doc", headers).await;
    let job_id = next_job_id(state, "baidu-doc");
    store_job(
        state,
        &job_id,
        "baidu-doc",
        json!({ "result": { "status": "Succeeded", "output": { "files": [{ "url": translated_media_url("document") }] } } }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "result": { "id": job_id } }))
}

async fn handle_baidu_cloud_doc_query(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "baidu-cloud-doc-query", headers).await;
    let value = parse_json(body);
    let job_id = value.get("id").and_then(Value::as_str).unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error_msg": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_youdao_image(state: &AppState, body: &str) -> Response {
    let form = parse_form(body);
    let app_key = form.get("appKey").cloned().unwrap_or_default();
    let stat_key = format!("official:youdao:{}", &app_key);
    let _ = key_enter(state, &stat_key).await;
    let params = verify::YoudaoParams {
        app_key: app_key.clone(),
        q: form.get("q").cloned().unwrap_or_default(),
        salt: form.get("salt").cloned().unwrap_or_default(),
        curtime: form.get("curtime").cloned().unwrap_or_default(),
        sign: form.get("sign").cloned().unwrap_or_default(),
        sign_type: form.get("signType").cloned().unwrap_or_default(),
    };
    if app_key.is_empty() || app_key != crate::config::YOUDAO_APP_KEY {
        key_exit(state, &stat_key).await;
        return json_response(json!({ "errorCode": "108" }));
    }
    if let Err(_error) = verify::verify_youdao(&params) {
        key_exit(state, &stat_key).await;
        return json_response(json!({ "errorCode": "202" }));
    }
    let to = form.get("to").cloned().unwrap_or_else(|| "zh-CHS".into());
    let from = form.get("from").cloned().unwrap_or_else(|| "auto".into());
    let translated = build_text("image text", &to);
    let response = json_response(json!({
        "errorCode": "0",
        "Result": {
            "render_image": translated_media_url("image"),
            "orientation": "0",
            "lanFrom": from,
            "lanTo": to,
            "content": [{ "src": "image text", "dst": translated }]
        }
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_youdao_speech(state: &AppState, body: &str) -> Response {
    let form = parse_form(body);
    let stat_key = format!("official:youdao:{}", form.get("appKey").cloned().unwrap_or_default());
    let _ = key_enter(state, &stat_key).await;
    let params = verify::YoudaoParams {
        app_key: form.get("appKey").cloned().unwrap_or_default(),
        q: form.get("q").cloned().unwrap_or_default(),
        salt: form.get("salt").cloned().unwrap_or_default(),
        curtime: form.get("curtime").cloned().unwrap_or_default(),
        sign: form.get("sign").cloned().unwrap_or_default(),
        sign_type: form.get("signType").cloned().unwrap_or_default(),
    };
    if let Err(error) = verify::verify_youdao(&params) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let translated = build_text("speech text", form.get("to").map(String::as_str).unwrap_or("en"));
    let response = json_response(json!({ "translation": [translated] }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_youdao_doc(state: &AppState, body: &str) -> Response {
    let form = parse_form(body);
    let stat_key = format!("official:youdao:{}", form.get("appKey").cloned().unwrap_or_default());
    let _ = key_enter(state, &stat_key).await;
    let params = verify::YoudaoParams {
        app_key: form.get("appKey").cloned().unwrap_or_default(),
        q: form.get("file").cloned().unwrap_or_default(),
        salt: form.get("salt").cloned().unwrap_or_default(),
        curtime: form.get("curtime").cloned().unwrap_or_default(),
        sign: form.get("sign").cloned().unwrap_or_default(),
        sign_type: form.get("signType").cloned().unwrap_or_default(),
    };
    if let Err(error) = verify::verify_youdao(&params) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let response = json_response(json!({ "data": { "taskNo": next_job_id(state, "youdao-doc") } }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_youdao_html(state: &AppState, body: &str) -> Response {
    let form = parse_form(body);
    let stat_key = format!("official:youdao:{}", form.get("appKey").cloned().unwrap_or_default());
    let _ = key_enter(state, &stat_key).await;
    let params = verify::YoudaoParams {
        app_key: form.get("appKey").cloned().unwrap_or_default(),
        q: form.get("q").cloned().unwrap_or_default(),
        salt: form.get("salt").cloned().unwrap_or_default(),
        curtime: form.get("curtime").cloned().unwrap_or_default(),
        sign: form.get("sign").cloned().unwrap_or_default(),
        sign_type: form.get("signType").cloned().unwrap_or_default(),
    };
    if let Err(error) = verify::verify_youdao(&params) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let translated = build_text(form.get("q").map(String::as_str).unwrap_or_default(), form.get("to").map(String::as_str).unwrap_or("en"));
    let response = json_response(json!({ "data": translated }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_apertium_text(state: &AppState, query: &HashMap<String, String>) -> Response {
    let _ = key_enter(state, "official:apertium").await;
    let translated = build_text(
        query.get("q").map(String::as_str).unwrap_or_default(),
        query.get("langpair").and_then(|pair| pair.split('|').nth(1)).unwrap_or("en"),
    );
    key_exit(state, "official:apertium").await;
    json_response(json!({ "responseData": { "translatedText": translated }, "responseDetails": null }))
}

async fn handle_apertium_doc(state: &AppState) -> Response {
    let _ = key_enter(state, "official:apertium-doc").await;
    key_exit(state, "official:apertium-doc").await;
    translated_binary_response("document", Some("apertium-translated.docx"), Some("application/octet-stream"))
}

async fn handle_deeplx(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "deeplx", headers).await;
    let value = parse_json(body);
    json_response(json!({ "data": build_text(value.get("text").and_then(Value::as_str).unwrap_or_default(), value.get("target_lang").and_then(Value::as_str).unwrap_or("en")) }))
}

async fn handle_cloudflare(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "cloudflare", headers).await;
    let value = parse_json(body);
    json_response(json!({ "result": { "translated_text": build_text(value.get("text").and_then(Value::as_str).unwrap_or_default(), value.get("target_lang").and_then(Value::as_str).unwrap_or("en")) } }))
}

async fn handle_huggingface_router(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "hf-router", headers).await;
    let value = parse_json(body);
    let target = value.get("parameters").and_then(|params| params.get("tgt_lang")).and_then(Value::as_str).unwrap_or("en");
    json_response(json!([{ "translation_text": build_text(value.get("inputs").and_then(Value::as_str).unwrap_or_default(), target) }]))
}

async fn handle_lilt_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "lilt-text", headers).await;
    let value = parse_json(body);
    json_response(json!({ "translation": [[{ "target": build_text(value.get("source").and_then(Value::as_str).unwrap_or_default(), extract_target_lang(value.get("source").and_then(Value::as_str).unwrap_or_default()).as_deref().unwrap_or("en")) }]] }))
}

async fn handle_lilt_file_upload(state: &AppState) -> Response {
    let file_id = next_job_id(state, "lilt-file");
    json_response(json!({ "id": file_id }))
}

async fn handle_lilt_file_submit(state: &AppState) -> Response {
    let job_id = next_job_id(state, "lilt-translation");
    store_job(
        state,
        &job_id,
        "lilt-file",
        json!([{ "id": job_id, "status": "completed" }]),
        Some("document"),
        Some("lilt-translated.docx".into()),
        Some("application/octet-stream"),
    ).await;
    json_response(json!([{ "id": job_id }]))
}

async fn handle_lilt_file_poll(state: &AppState, query: &HashMap<String, String>) -> Response {
    let job_id = query.get("translationIds").cloned().unwrap_or_default();
    let Some(job) = load_job(state, &job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_lilt_file_download(state: &AppState, query: &HashMap<String, String>) -> Response {
    let job_id = query.get("id").cloned().unwrap_or_default();
    let Some(job) = load_job(state, &job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "job not found" }))).into_response();
    };
    translated_binary_response(
        job.download_kind.as_deref().unwrap_or("document"),
        job.download_filename.as_deref(),
        job.download_content_type.as_deref(),
    )
}

async fn handle_translateplus_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "translateplus-text", headers).await;
    let value = parse_json(body);
    json_response(json!({ "translations": { "translation": build_text(value.get("text").and_then(Value::as_str).unwrap_or_default(), value.get("target").and_then(Value::as_str).unwrap_or("en")) } }))
}

async fn handle_translateplus_html(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "translateplus-html", headers).await;
    let value = parse_json(body);
    json_response(json!({ "html": build_text(value.get("html").and_then(Value::as_str).unwrap_or_default(), value.get("target").and_then(Value::as_str).unwrap_or("en")) }))
}

async fn handle_translateplus_email(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "translateplus-email", headers).await;
    let value = parse_json(body);
    json_response(json!({ "html_body": build_text(value.get("email_body").and_then(Value::as_str).unwrap_or_default(), value.get("target").and_then(Value::as_str).unwrap_or("en")) }))
}

async fn handle_translateplus_subtitles(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "translateplus-subtitles", headers).await;
    let value = parse_json(body);
    let _ = value;
    json_response(json!({
        "content": format!("{}/media/document-source.srt", crate::config::Config::from_env().base_url)
    }))
}

async fn handle_translateplus_i18n_submit(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "translateplus-i18n", headers).await;
    let job_id = next_job_id(state, "translateplus-i18n");
    store_job(
        state,
        &job_id,
        "translateplus-i18n",
        json!({ "status": "completed" }),
        Some("document"),
        Some("translateplus-i18n.json".into()),
        Some("application/octet-stream"),
    ).await;
    json_response(json!({ "job_id": job_id }))
}

async fn handle_translateplus_i18n_poll_or_download(state: &AppState, path: &str) -> Response {
    let suffix = path.trim_start_matches("/v2/translate/i18n/jobs/");
    if let Some((job_id, _)) = suffix.split_once("/download/") {
        let Some(job) = load_job(state, job_id).await else {
            return (StatusCode::NOT_FOUND, Json(json!({ "detail": "job not found" }))).into_response();
        };
        return translated_binary_response(
            job.download_kind.as_deref().unwrap_or("document"),
            job.download_filename.as_deref(),
            job.download_content_type.as_deref(),
        );
    }
    let Some(job) = load_job(state, suffix).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "detail": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_niutrans_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let _ = headers;
    let form = parse_form(body);
    let api_key = form
        .get("apikey")
        .or_else(|| form.get("api_key"))
        .cloned()
        .unwrap_or_default();
    let q = form
        .get("q")
        .or_else(|| form.get("src_text"))
        .cloned()
        .unwrap_or_default();
    let from = form.get("from").cloned().unwrap_or_else(|| "en".into());
    let to = form.get("to").cloned().unwrap_or_else(|| "zh".into());
    let sign = form.get("sign").cloned().unwrap_or_default();
    let stat_key = format!("official:niutrans:{api_key}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_niutrans(&verify::NiutransParams {
        api_key,
        q: q.clone(),
        from: from.clone(),
        to: to.clone(),
        sign,
    }) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let response = json_response(json!({ "tgt_text": build_text(&q, &to), "error_msg": "" }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_libretranslate_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let _ = headers;
    let value = parse_json(body);
    let api_key = value
        .get("api_key")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stat_key = format!("official:libretranslate:{api_key}");
    let _ = key_enter(state, &stat_key).await;
    if api_key != crate::config::BEARER_KEY && api_key != crate::config::GOOGLE_API_KEY {
        key_exit(state, &stat_key).await;
        return auth_error_response(verify::AuthError::new(
            "libretranslate",
            format!(
                "Invalid LibreTranslate api_key: expected {} (or {}), got {}",
                crate::config::BEARER_KEY,
                crate::config::GOOGLE_API_KEY,
                if api_key.is_empty() {
                    "<empty>"
                } else {
                    api_key
                }
            ),
        ));
    }
    let text = value.get("q").and_then(Value::as_str).unwrap_or_default();
    let target = value.get("target").and_then(Value::as_str).unwrap_or("en");
    let response = json_response(json!({ "translatedText": build_text(text, target) }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_tilde_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "tilde-text", headers).await;
    let value = parse_json(body);
    let target = value.get("trgLang").and_then(Value::as_str).unwrap_or("en");
    let text = value.get("text").and_then(Value::as_array).and_then(|items| items.first()).and_then(Value::as_str).unwrap_or_default();
    json_response(json!({ "translations": [{ "translation": build_text(text, target) }] }))
}

async fn handle_tilde_file_submit(state: &AppState, headers: &HeaderMap, content_type: &str, body: &[u8]) -> Response {
    simple_stat(state, "tilde-file", headers).await;
    let payload = parse_multipart(content_type, body);
    let file_name = payload
        .filenames
        .get("file")
        .cloned()
        .unwrap_or_else(|| "source.bin".into());
    let media_kind = if file_name.ends_with(".png") || file_name.ends_with(".jpg") || file_name.ends_with(".jpeg") {
        "image"
    } else {
        "document"
    };
    let job_id = next_job_id(state, "tilde");
    store_job(
        state,
        &job_id,
        "tilde-file",
        json!({ "status": "Completed", "files": [{ "id": "source" }, { "id": "translated-file" }] }),
        Some(media_kind),
        Some(file_name),
        Some("application/octet-stream"),
    ).await;
    json_response(json!({ "id": job_id }))
}

async fn handle_tilde_file_poll_or_download(state: &AppState, path: &str) -> Response {
    let suffix = path.trim_start_matches("/api/translate/file/");
    let parts: Vec<&str> = suffix.split('/').collect();
    let Some(job_id) = parts.first().copied() else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid path" }))).into_response();
    };
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "job not found" } }))).into_response();
    };
    if parts.len() >= 2 {
        return translated_binary_response(
            job.download_kind.as_deref().unwrap_or("document"),
            job.download_filename.as_deref(),
            job.download_content_type.as_deref(),
        );
    }
    json_response(job.poll_response)
}

async fn handle_doctranslate_text_submit(state: &AppState, headers: &HeaderMap, content_type: &str, body: &[u8]) -> Response {
    simple_stat(state, "doctranslate-text", headers).await;
    let payload = parse_multipart(content_type, body);
    let target = payload.fields.get("dest_lang").map(String::as_str).unwrap_or("en");
    let text = payload.fields.get("text").map(String::as_str).unwrap_or("text");
    let job_id = next_job_id(state, "doctranslate-text");
    store_job(
        state,
        &job_id,
        "doctranslate-text",
        json!({ "data": { job_id.clone(): { "status": "completed", "translated_text": build_text(text, target) } } }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "data": { "task_id": job_id } }))
}

async fn handle_doctranslate_v3_submit(state: &AppState, headers: &HeaderMap, content_type: &str, body: &[u8]) -> Response {
    simple_stat(state, "doctranslate-v3", headers).await;
    let payload = parse_multipart(content_type, body);
    let task_type = payload.fields.get("task_type").cloned().unwrap_or_else(|| "Document Translation".into());
    let media_kind = if task_type.contains("Audio") {
        "audio"
    } else if task_type.contains("Image") {
        "image"
    } else if task_type.contains("Video") {
        "video"
    } else {
        "document"
    };
    let job_id = next_job_id(state, "doctranslate-v3");
    let download_url = translated_media_url(media_kind);
    store_job(
        state,
        &job_id,
        "doctranslate-v3",
        json!({ "status": "completed", "url_download": download_url }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "task_id": job_id, "url_download": download_url }))
}

async fn handle_doctranslate_v3_result(state: &AppState, path: &str) -> Response {
    let job_id = path.trim_start_matches("/v3/result/");
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_doctranslate_video_upload(state: &AppState) -> Response {
    let file_task_id = next_job_id(state, "doctranslate-video-file");
    json_response(json!({ "data": [{ "task_id": file_task_id.clone(), "url_download": translated_media_url("video") }] }))
}

async fn handle_doctranslate_video_process(state: &AppState, body: &str) -> Response {
    let value = parse_json(body);
    let meta_files = value.get("meta_files").cloned().unwrap_or_else(|| json!([{"task_id":"video-file"}]));
    let file_task_id = meta_files
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item.get("task_id"))
        .and_then(Value::as_str)
        .unwrap_or("video-file")
        .to_string();
    let job_id = next_job_id(state, "doctranslate-video");
    store_job(
        state,
        &job_id,
        "doctranslate-video",
        json!({ "data": { file_task_id.clone(): { "status": "completed", "url_download": translated_media_url("video") } } }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "data": { "task_id": job_id, file_task_id: { "url_download": translated_media_url("video") } } }))
}

async fn handle_doctranslate_v1_result(state: &AppState, path: &str) -> Response {
    let job_id = path.trim_start_matches("/v1/result/");
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error_description": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_cloudmersive_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "cloudmersive", headers).await;
    let value = parse_json(body);
    json_response(json!({ "TranslatedResult": build_text(value.get("Text").and_then(Value::as_str).unwrap_or_default(), "en") }))
}

async fn handle_google_v2_like(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &str,
) -> Response {
    let value = parse_json(body);
    let query_key = query.get("key").cloned();
    let header_key = header_value(headers, "x-goog-api-key");
    let body_key = value.get("key").and_then(Value::as_str).map(str::to_string);

    let (api_key, is_body_only) = if let Some(k) = header_key.or(query_key) {
        (k, false)
    } else if let Some(k) = body_key {
        (k, true)
    } else {
        (String::new(), false)
    };

    let stat_key = format!(
        "official:google-v2-like:{}",
        if api_key.is_empty() {
            "none".into()
        } else {
            api_key.clone()
        }
    );
    let _ = key_enter(state, &stat_key).await;

    if is_body_only {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": {
                    "code": 403,
                    "message": "Requests from this client are unauthorized (unregistered callers). Please pass API key via query parameter 'key' or header 'X-Goog-Api-Key'.",
                    "status": "PERMISSION_DENIED",
                    "details": [
                        {
                            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                            "reason": "API_KEY_INVALID"
                        }
                    ]
                }
            })),
        )
            .into_response();
    }

    if let Err(_error) = verify::verify_google_api_key(&api_key) {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": {
                    "code": 403,
                    "message": "The request is missing a valid API key.",
                    "status": "PERMISSION_DENIED",
                    "details": [
                        {
                            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                            "reason": "API_KEY_INVALID"
                        }
                    ]
                }
            })),
        )
            .into_response();
    }

    let target = value.get("target").and_then(Value::as_str).unwrap_or("en");
    let source = value.get("source").and_then(Value::as_str);
    let is_source_valid = match source {
        None => true,
        Some(s) if s.is_empty() || s == "auto" => true,
        Some(s) => crate::languages::is_valid_google_lang(s),
    };

    if !crate::languages::is_valid_google_lang(target) || !is_source_valid {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "code": 400,
                    "message": "Invalid Value",
                    "status": "INVALID_ARGUMENT",
                    "details": [
                        {
                            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                            "reason": "INVALID_ARGUMENT"
                        }
                    ]
                }
            })),
        )
            .into_response();
    }

    let text = value
        .get("q")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .or_else(|| value.get("q").and_then(Value::as_str))
        .unwrap_or_default();
    if let Some(resp) = crate::auth_profiles::reject_if_too_long(text, 30_000) {
        key_exit(state, &stat_key).await;
        return resp;
    }
    let response = json_response(json!({
        "data": {
            "translations": [{
                "translatedText": build_text(text, target),
                "detectedSourceLanguage": source.unwrap_or("en")
            }]
        }
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_youdao_text(state: &AppState, body: &str) -> Response {
    let form = parse_form(body);
    let app_key = form.get("appKey").cloned().unwrap_or_default();
    let q = form.get("q").cloned().unwrap_or_default();
    let salt = form.get("salt").cloned().unwrap_or_default();
    let curtime = form.get("curtime").cloned().unwrap_or_default();
    let sign = form.get("sign").cloned().unwrap_or_default();
    let sign_type = form.get("signType").cloned().unwrap_or_default();
    let from = form.get("from").cloned().unwrap_or_else(|| "en".into());
    let to = form.get("to").cloned().unwrap_or_else(|| "en".into());

    let stat_key = format!("official:youdao:{}", &app_key);
    let _ = key_enter(state, &stat_key).await;

    if app_key.is_empty() || app_key != crate::config::YOUDAO_APP_KEY {
        key_exit(state, &stat_key).await;
        return json_response(json!({ "errorCode": "108" }));
    }
    if q.is_empty() {
        key_exit(state, &stat_key).await;
        return json_response(json!({ "errorCode": "101" }));
    }
    if !crate::languages::is_valid_youdao_code(&from) || !crate::languages::is_valid_youdao_code(&to) {
        key_exit(state, &stat_key).await;
        return json_response(json!({ "errorCode": "102" }));
    }
    let params = verify::YoudaoParams {
        app_key,
        q: q.clone(),
        salt,
        curtime,
        sign,
        sign_type,
    };
    if let Err(_error) = verify::verify_youdao(&params) {
        key_exit(state, &stat_key).await;
        return json_response(json!({ "errorCode": "202" }));
    }

    let translated = build_text(q.as_str(), &to);
    let response = json_response(json!({
        "errorCode": "0",
        "query": q,
        "l": format!("{from}-2-{to}"),
        "isWord": false,
        "translation": [translated]
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_modernmt(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let mmt_key = header_value(headers, "mmt-apikey").unwrap_or_default();
    let stat_key = format!("official:modernmt:{}", if authorization.is_empty() { &mmt_key } else { &authorization });
    let _ = key_enter(state, &stat_key).await;
    let auth_ok = if !authorization.is_empty() {
        verify::verify_bearer_api_key(&authorization).is_ok()
    } else if !mmt_key.is_empty() {
        mmt_key == crate::config::BEARER_KEY
    } else {
        false
    };
    if !auth_ok {
        key_exit(state, &stat_key).await;
        return auth_error_response(verify::AuthError::new(
            "modernmt",
            "Missing or invalid ModernMT Authorization / MMApiKey",
        ));
    }
    let value = parse_json(body);
    let text = value
        .get("q")
        .or_else(|| value.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target = value.get("target").and_then(Value::as_str).unwrap_or("en");
    let response = json_response(json!({
        "data": {
            "translation": build_text(text, target)
        },
        // Argos/Bergamot catalog templates expect LibreTranslate-shaped top-level field
        // when they share Bearer+/translate with ModernMT on the mock.
        "translatedText": build_text(text, target)
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_niutrans_upload(state: &AppState, kind: &str) -> Response {
    let job_id = next_job_id(state, &format!("niutrans-{kind}"));
    store_job(
        state,
        &job_id,
        &format!("niutrans-{kind}"),
        json!({ "data": { "fileNo": job_id.clone(), "transStatus": "105" } }),
        Some(kind),
        Some(format!("niutrans-{kind}-translated.bin")),
        Some("application/octet-stream"),
    ).await;
    json_response(json!({ "data": { "fileNo": job_id } }))
}

async fn handle_niutrans_status(state: &AppState, path: &str) -> Response {
    let job_id = path.rsplit('/').next().unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "msg": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_niutrans_download(state: &AppState, path: &str, kind: &str) -> Response {
    let job_id = path.rsplit('/').next().unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "msg": "job not found" }))).into_response();
    };
    translated_binary_response(
        job.download_kind.as_deref().unwrap_or(kind),
        job.download_filename.as_deref(),
        job.download_content_type.as_deref(),
    )
}

async fn handle_systran_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:systran-text:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_bearer_api_key(&authorization) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let value = parse_json(body);
    let response = json_response(json!({
        "outputs": [{
            "output": build_text(
                value.get("input").and_then(Value::as_str).unwrap_or_default(),
                value.get("target").and_then(Value::as_str).unwrap_or("en")
            )
        }]
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_systran_file_submit(state: &AppState, headers: &HeaderMap, content_type: &str, body: &[u8]) -> Response {
    simple_stat(state, "systran-file", headers).await;
    let payload = parse_multipart(content_type, body);
    let filename = payload
        .filenames
        .get("input")
        .cloned()
        .unwrap_or_else(|| "systran-translated.bin".into());
    let media_kind = if filename.ends_with(".png") || filename.ends_with(".jpg") || filename.ends_with(".jpeg") {
        "image"
    } else {
        "document"
    };
    let job_id = next_job_id(state, "systran-file");
    store_job(
        state,
        &job_id,
        "systran-file",
        json!({ "requestId": job_id.clone(), "status": "finished" }),
        Some(media_kind),
        Some(filename),
        Some("application/octet-stream"),
    ).await;
    json_response(json!({ "requestId": job_id }))
}

async fn handle_systran_file_status(state: &AppState, query: &HashMap<String, String>) -> Response {
    let job_id = query.get("requestId").cloned().unwrap_or_default();
    let Some(job) = load_job(state, &job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "job not found" } }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_systran_file_result(state: &AppState, query: &HashMap<String, String>) -> Response {
    let job_id = query.get("requestId").cloned().unwrap_or_default();
    let Some(job) = load_job(state, &job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "job not found" } }))).into_response();
    };
    translated_binary_response(
        job.download_kind.as_deref().unwrap_or("document"),
        job.download_filename.as_deref(),
        job.download_content_type.as_deref(),
    )
}

async fn handle_ibm_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:ibm-text:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    let auth_ok = if authorization.to_ascii_lowercase().starts_with("basic ") {
        authorization.contains(crate::config::BEARER_KEY)
    } else {
        verify::verify_bearer_api_key(&authorization).is_ok()
    };
    if !auth_ok {
        key_exit(state, &stat_key).await;
        return auth_error_response(verify::AuthError::new(
            "ibm",
            "Missing or invalid IBM Watson Authorization (Bearer or Basic with mock key)",
        ));
    }
    let value = parse_json(body);
    let text = value
        .get("text")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .unwrap_or_default();
    let response = json_response(json!({
        "translations": [{
            "translation": build_text(text, value.get("target").and_then(Value::as_str).unwrap_or("en"))
        }]
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_ibm_document_submit(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "ibm-document", headers).await;
    let job_id = next_job_id(state, "ibm-document");
    store_job(
        state,
        &job_id,
        "ibm-document",
        json!({ "document_id": job_id.clone(), "status": "available" }),
        Some("document"),
        Some("ibm-watson-translated.bin".into()),
        Some("application/octet-stream"),
    ).await;
    json_response(json!({ "document_id": job_id }))
}

async fn handle_ibm_document_poll(state: &AppState, path: &str) -> Response {
    let suffix = path.split("/v3/documents/").nth(1).unwrap_or_default();
    let job_id = suffix.split('?').next().unwrap_or_default().split('/').next().unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "errors": [{ "message": "job not found" }] }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_ibm_document_download(state: &AppState, path: &str) -> Response {
    let suffix = path.split("/v3/documents/").nth(1).unwrap_or_default();
    let job_id = suffix.split('?').next().unwrap_or_default().split('/').next().unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "errors": [{ "message": "job not found" }] }))).into_response();
    };
    translated_binary_response(
        job.download_kind.as_deref().unwrap_or("document"),
        job.download_filename.as_deref(),
        job.download_content_type.as_deref(),
    )
}

async fn handle_caiyun_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "caiyun", headers).await;
    let value = parse_json(body);
    let target = value.get("trans_type").and_then(Value::as_str).and_then(|s| s.split('2').nth(1)).unwrap_or("en");
    let text = value
        .get("source")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .or_else(|| value.get("source").and_then(Value::as_str))
        .unwrap_or_default();
    json_response(json!({ "target": [build_text(text, target)] }))
}

async fn handle_lingvanex_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:lingvanex-text:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_bearer_api_key(&authorization) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let value = parse_json(body);
    let response = json_response(json!({
        "result": build_text(
            value
                .get("data")
                .and_then(Value::as_str)
                .or_else(|| value.get("text").and_then(Value::as_str))
                .or_else(|| value.get("q").and_then(Value::as_str))
                .unwrap_or_default(),
            value
                .get("to")
                .or_else(|| value.get("target"))
                .and_then(Value::as_str)
                .unwrap_or("en")
        )
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_mymemory_text(state: &AppState, query: &HashMap<String, String>) -> Response {
    let _ = key_enter(state, "official:mymemory").await;
    let translated = build_text(
        query.get("q").map(String::as_str).unwrap_or_default(),
        query.get("langpair").and_then(|pair| pair.split('|').nth(1)).unwrap_or("en"),
    );
    key_exit(state, "official:mymemory").await;
    json_response(json!({ "responseData": { "translatedText": translated }, "responseDetails": "" }))
}

async fn handle_eden_ai(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "eden-ai", headers).await;
    let value = parse_json(body);
    let model = value.get("model").and_then(Value::as_str).unwrap_or_default();
    if model.contains("document_translation") {
        return json_response(json!({ "output": { "document_resource_url": translated_media_url("document") } }));
    }
    let target_lang = value
        .get("target_language")
        .and_then(Value::as_str)
        .or_else(|| value.get("target_lang").and_then(Value::as_str))
        .or_else(|| value.get("to").and_then(Value::as_str))
        .unwrap_or("en");
    let source_text = value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.get("input").and_then(Value::as_str))
        .unwrap_or("eden text");
    let translated = build_text(source_text, target_lang);
    let translated = if source_text.contains('<') && source_text.contains('>') {
        translated
    } else {
        format!("<p>{}</p>", translated)
    };
    json_response(json!({ "output": { "translated_text": translated } }))
}

async fn handle_did_submit(state: &AppState, headers: &HeaderMap, _body: &str) -> Response {
    simple_stat(state, "did-video", headers).await;
    let job_id = next_job_id(state, "did-video");
    store_job(
        state,
        &job_id,
        "did-video",
        json!({ "id": job_id.clone(), "status": "done", "result_url": translated_media_url("video") }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "translations": [{ "id": job_id }] }))
}

async fn handle_did_poll(state: &AppState, path: &str) -> Response {
    let job_id = path.trim_start_matches("/translations/");
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_bedrock_converse(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "bedrock-converse", headers).await;
    let value = parse_json(body);
    let text = value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.first())
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    json_response(json!({ "output": { "message": { "content": [{ "text": build_text(text, "en") }] } } }))
}

async fn handle_dashscope_qwen_image_submit(state: &AppState, headers: &HeaderMap, _body: &str) -> Response {
    simple_stat(state, "dashscope-qwen-image", headers).await;
    let job_id = next_job_id(state, "dashscope-qwen-image");
    store_job(
        state,
        &job_id,
        "dashscope-qwen-image",
        json!({
            "output": {
                "task_id": job_id.clone(),
                "task_status": "SUCCEEDED",
                "results": [{ "url": translated_media_url("image") }]
            }
        }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "output": { "task_id": job_id } }))
}

async fn handle_dashscope_qwen_image_poll(state: &AppState, path: &str) -> Response {
    let job_id = path.trim_start_matches("/api/v1/tasks/");
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "code": "NotFound" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_volcengine_official(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &str,
) -> Response {
    if *method != Method::GET {
        let authorization = header_value(headers, "authorization").unwrap_or_default();
        let amz_date = header_value(headers, "x-amz-date").unwrap_or_default();
        let host = header_value(headers, "host").unwrap_or_default();
        let params = verify::AwsSigV4Params {
            authorization,
            amz_date,
            body: body.to_string(),
            host,
            path: "/".to_string(),
        };
        if let Err(error) = verify::verify_volcengine(&params) {
            return auth_error_response(error);
        }
    }
    let action = query.get("Action").map(String::as_str).unwrap_or_default();
    let value = parse_json(body);
    match action {
        "TranslateText" => json_response(json!({
            "TranslationList": [{
                "Translation": build_text(
                    value.get("TextList").and_then(Value::as_array).and_then(|items| items.first()).and_then(Value::as_str).unwrap_or_default(),
                    value.get("TargetLanguage").and_then(Value::as_str).unwrap_or("en")
                )
            }]
        })),
        "TranslateImage" => json_response(json!({ "Image": translated_media_url("image") })),
        "DocumentCreate" => json_response(json!({ "data": { "projectId": next_job_id(state, "volc-project") } })),
        "DocumentTaskCreate" => {
            let job_id = next_job_id(state, "volc-doc");
            store_job(
                state,
                &job_id,
                "volc-doc",
                json!({
                    "data": {
                        "list": [{
                            "state": "3",
                            "subtasks": [{ "targetDocUrl": translated_media_url("document") }]
                        }]
                    }
                }),
                None,
                None,
                None,
            ).await;
            json_response(json!({ "data": { "taskId": job_id } }))
        }
        "DocumentTaskDetail" => {
            let job_id = query.get("taskId").cloned().unwrap_or_default();
            let Some(job) = load_job(state, &job_id).await else {
                return (StatusCode::NOT_FOUND, Json(json!({ "ResponseMetadata": { "Error": { "Message": "job not found" } } }))).into_response();
            };
            json_response(job.poll_response)
        }
        "SubmitAITranslationWorkflow" => {
            let job_id = next_job_id(state, "volc-video");
            store_job(
                state,
                &job_id,
                "volc-video",
                json!({
                    "Result": {
                        "ProjectBaseInfo": { "ProjectId": job_id.clone() },
                        "ProjectInfo": {
                            "Status": "ExportSucceed",
                            "OutputVideo": { "Url": translated_media_url("video") }
                        }
                    }
                }),
                None,
                None,
                None,
            ).await;
            json_response(json!({ "Result": { "ProjectBaseInfo": { "ProjectId": job_id } } }))
        }
        "GetAITranslationProject" => {
            let job_id = query.get("ProjectId").cloned().unwrap_or_default();
            let Some(job) = load_job(state, &job_id).await else {
                return (StatusCode::NOT_FOUND, Json(json!({ "ResponseMetadata": { "Error": { "Message": "job not found" } } }))).into_response();
            };
            json_response(job.poll_response)
        }
        _ => json_response(json!({ "ResponseMetadata": { "Error": { "Message": "unsupported action" } } })),
    }
}

async fn handle_baidu_open_text(state: &AppState, body: &str) -> Response {
    let form = parse_form(body);
    let appid = form.get("appid").cloned().unwrap_or_default();
    let q = form.get("q").cloned().unwrap_or_default();
    let salt = form.get("salt").cloned().unwrap_or_default();
    let sign = form.get("sign").cloned().unwrap_or_default();
    let from = form.get("from").cloned().unwrap_or_else(|| "en".into());
    let to = form.get("to").cloned().unwrap_or_else(|| "en".into());

    let stat_key = format!("official:baidu:{}", &appid);
    let _ = key_enter(state, &stat_key).await;

    if appid.is_empty() || appid != crate::config::BAIDU_APPID {
        key_exit(state, &stat_key).await;
        return json_response(json!({
            "error_code": "52003",
            "error_msg": "UNAUTHORIZED USER"
        }));
    }
    if q.is_empty() {
        key_exit(state, &stat_key).await;
        return json_response(json!({
            "error_code": "54000",
            "error_msg": "PARAM_EMPTY"
        }));
    }
    if !crate::languages::is_valid_baidu_code(&to) {
        key_exit(state, &stat_key).await;
        return json_response(json!({
            "error_code": "58001",
            "error_msg": "INVALID_TO_PARAM"
        }));
    }
    if !crate::languages::is_valid_baidu_code(&from) {
        key_exit(state, &stat_key).await;
        return json_response(json!({
            "error_code": "58000",
            "error_msg": "INVALID_FROM_PARAM"
        }));
    }
    let params = verify::BaiduParams {
        appid,
        q: q.clone(),
        salt,
        sign,
    };
    if let Err(_error) = verify::verify_baidu(&params) {
        key_exit(state, &stat_key).await;
        return json_response(json!({
            "error_code": "54001",
            "error_msg": "SIGN_ERROR"
        }));
    }

    let trans_result: Vec<Value> = q
        .split('\n')
        .map(|line| {
            let dst = if line.trim().is_empty() {
                String::new()
            } else {
                build_text(line, &to)
            };
            json!({ "src": line, "dst": dst })
        })
        .collect();
    let response = json_response(json!({
        "from": from,
        "to": to,
        "trans_result": trans_result
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_iflytek_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let app_id = header_value(headers, "x-appid").unwrap_or_default();
    let cur_time = header_value(headers, "x-curtime").unwrap_or_default();
    let checksum = header_value(headers, "x-checksum").unwrap_or_default();
    let x_param = header_value(headers, "x-param").unwrap_or_default();
    let stat_key = format!("official:iflytek:{app_id}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_iflytek(&verify::IflytekParams {
        app_id,
        cur_time,
        checksum,
        param: x_param,
        body: body.to_string(),
    }) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let value = parse_json(body);
    let text = value
        .get("data")
        .and_then(|data| data.get("text"))
        .and_then(Value::as_str)
        .or_else(|| value.get("text").and_then(Value::as_str))
        .unwrap_or_default();
    let target = value
        .get("business")
        .and_then(|business| business.get("to"))
        .and_then(Value::as_str)
        .or_else(|| value.get("to").and_then(Value::as_str))
        .unwrap_or("en");
    // Catalog path: data.trans_result.dst
    let response = json_response(json!({
        "data": {
            "trans_result": { "dst": build_text(text, target) },
            "result": { "trans_result": { "dst": build_text(text, target) } }
        }
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_reverso_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let api_key = header_value(headers, "api-key").unwrap_or_default();
    let stat_key = format!("official:reverso:{api_key}");
    let _ = key_enter(state, &stat_key).await;
    if api_key != crate::config::BEARER_KEY {
        key_exit(state, &stat_key).await;
        return auth_error_response(verify::AuthError::new(
            "reverso",
            format!("Invalid Api-Key: expected {}", crate::config::BEARER_KEY),
        ));
    }
    let value = parse_json(body);
    let text = value
        .get("q")
        .or_else(|| value.get("text"))
        .or_else(|| value.get("input"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target = value
        .get("target")
        .or_else(|| value.get("to"))
        .and_then(Value::as_str)
        .unwrap_or("zh");
    let response = json_response(json!({ "translation": [build_text(text, target)] }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_unbabel_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:unbabel:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_bearer_api_key(&authorization) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let value = parse_json(body);
    let text = value
        .get("text")
        .or_else(|| value.get("q"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target = value
        .get("target_lang")
        .or_else(|| value.get("target"))
        .and_then(Value::as_str)
        .unwrap_or("zh");
    let response = json_response(json!({ "translated_text": build_text(text, target) }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_cloudtranslation_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:cloudtranslation:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_bearer_api_key(&authorization) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let value = parse_json(body);
    let text = value
        .get("text")
        .or_else(|| value.get("q"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target = value
        .get("target")
        .or_else(|| value.get("to"))
        .or_else(|| value.get("target_lang"))
        .and_then(Value::as_str)
        .unwrap_or("zh");
    // Catalog: translations.0.text
    let response = json_response(json!({
        "translations": [{ "text": build_text(text, target) }]
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_synthesia_asset_prepare(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "synthesia-asset", headers).await;
    let asset_id = next_job_id(state, "synthesia-asset");
    json_response(json!({
        "id": asset_id,
        "uploadCredentials": {
            "accessKeyId": "mock-upload-access-key",
            "bucket": "mock-synthesia-bucket",
            "key": format!("{asset_id}.bin"),
            "secretAccessKey": "mock-upload-secret",
            "sessionToken": "mock-upload-session"
        }
    }))
}

async fn handle_synthesia_dubbing_submit(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "synthesia-dubbing", headers).await;
    let job_id = next_job_id(state, "synthesia-dubbing");
    store_job(
        state,
        &job_id,
        "synthesia-dubbing",
        json!({
            "status": "complete",
            "dubbedAssets": [{ "downloadUrl": translated_media_url("video") }]
        }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "createdImportedAsset": { "id": job_id } }))
}

async fn handle_synthesia_dubbing_poll(state: &AppState, path: &str) -> Response {
    let job_id = path.split("/v2/dubbing/").nth(1).unwrap_or_default().split('?').next().unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "errorCode": "not_found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_elevenlabs_dubbing_submit(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "elevenlabs-dubbing", headers).await;
    let job_id = next_job_id(state, "elevenlabs-dubbing");
    store_job(
        state,
        &job_id,
        "elevenlabs-dubbing",
        json!({
            "dubbing_id": job_id.clone(),
            "status": "done",
            "result": { "video_url": translated_media_url("video") }
        }),
        Some("audio"),
        Some("elevenlabs-dubbing.mp3".into()),
        Some("audio/mpeg"),
    ).await;
    json_response(json!({ "dubbing_id": job_id }))
}

async fn handle_elevenlabs_dubbing_poll(state: &AppState, path: &str) -> Response {
    let job_id = path.split("/v1/dubbing/").nth(1).unwrap_or_default().split('?').next().unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "detail": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_elevenlabs_dubbing_download(state: &AppState, path: &str) -> Response {
    let job_id = path.split("/v1/dubbing/").nth(1).unwrap_or_default().split('/').next().unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "detail": "job not found" }))).into_response();
    };
    translated_binary_response(
        job.download_kind.as_deref().unwrap_or("audio"),
        job.download_filename.as_deref(),
        job.download_content_type.as_deref(),
    )
}

async fn handle_heygen_submit(state: &AppState, headers: &HeaderMap) -> Response {
    let api_key = header_value(headers, "x-api-key").unwrap_or_default();
    let stat_key = format!("official:heygen:{}", if api_key.is_empty() { "none" } else { &api_key });
    let _ = key_enter(state, &stat_key).await;

    if api_key.is_empty() || api_key.contains("bad") || api_key.contains("invalid") || (api_key != crate::config::BEARER_KEY && api_key != "mock-heygen-key") {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Unauthorized: Invalid API Key" })),
        )
            .into_response();
    }

    let job_id = next_job_id(state, "heygen-video");
    store_job(
        state,
        &job_id,
        "heygen-video",
        json!({
            "data": {
                "status": "processing",
                "progress": 50
            }
        }),
        None,
        None,
        None,
    ).await;
    key_exit(state, &stat_key).await;
    json_response(json!({ "data": { "video_translate_id": job_id } }))
}

async fn handle_heygen_poll(state: &AppState, path: &str) -> Response {
    let job_id = if path.contains("/v1/video_translate/") {
        let suffix = path.split("/v1/video_translate/").nth(1).unwrap_or_default();
        suffix.trim_end_matches("/status").split('?').next().unwrap_or_default()
    } else if path.starts_with("/v2/video_translate/") {
        path.trim_start_matches("/v2/video_translate/").split('?').next().unwrap_or_default()
    } else {
        "heygen-video"
    };

    let Some(mut job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "job not found" } }))).into_response();
    };

    let count = job.poll_count;
    job.poll_count += 1;
    state.official_jobs.lock().await.insert(job_id.to_string(), job.clone());

    if count == 0 {
        json_response(json!({
            "data": {
                "status": "processing",
                "progress": 50
            }
        }))
    } else {
        json_response(json!({
            "data": {
                "status": "success",
                "progress": 100,
                "video_url": translated_media_url("video"),
                "subtitle_url": translated_media_url("document")
            }
        }))
    }
}

async fn handle_rask_media_prepare(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "rask-media", headers).await;
    json_response(json!({ "id": next_job_id(state, "rask-media") }))
}

async fn handle_rask_project_submit(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "rask-project", headers).await;
    let value = parse_json(body);
    let translated_audio = translated_media_url("audio");
    let translated_video = translated_media_url("video");
    let status = if value.get("video_id").is_some() || value.get("media_id").is_some() {
        "merging_done"
    } else {
        "voiceover_done"
    };
    let job_id = next_job_id(state, "rask-project");
    store_job(
        state,
        &job_id,
        "rask-project",
        json!({
            "id": job_id.clone(),
            "status": status,
            "translated_audio": translated_audio,
            "translated_video": translated_video
        }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "id": job_id }))
}

async fn handle_rask_project_poll(state: &AppState, path: &str) -> Response {
    let job_id = path.split("/v2/projects/").nth(1).unwrap_or_default().split('?').next().unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "detail": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_reap_prepare(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "reap-prepare", headers).await;
    let upload_id = next_job_id(state, "reap-upload");
    json_response(json!({
        "uploadId": upload_id.clone(),
        "uploadUrl": format!("http://127.0.0.1:9090/mock-upload/{upload_id}")
    }))
}

async fn handle_reap_submit(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "reap-submit", headers).await;
    let job_id = next_job_id(state, "reap-project");
    store_job(
        state,
        &job_id,
        "reap-project",
        json!({ "id": job_id.clone(), "status": "completed" }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "id": job_id }))
}

async fn handle_reap_status(state: &AppState, query: &HashMap<String, String>) -> Response {
    let job_id = query.get("projectId").cloned().unwrap_or_default();
    let Some(job) = load_job(state, &job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_reap_clips(state: &AppState, query: &HashMap<String, String>) -> Response {
    let job_id = query.get("projectId").cloned().unwrap_or_default();
    if load_job(state, &job_id).await.is_none() {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "job not found" }))).into_response();
    }
    json_response(json!({ "clips": [{ "clipUrl": translated_media_url("video") }] }))
}

async fn handle_cloudmersive_document() -> Response {
    translated_binary_response("document", Some("cloudmersive-translated.docx"), Some("application/octet-stream"))
}

async fn handle_huawei_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:huawei-text:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_bearer_api_key(&authorization) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let value = parse_json(body);
    let text = value
        .get("text")
        .or_else(|| value.get("q"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target = value
        .get("to")
        .or_else(|| value.get("target"))
        .and_then(Value::as_str)
        .unwrap_or("zh");
    // Catalog: translations.0.text
    let response = json_response(json!({
        "translations": [{ "text": build_text(text, target) }],
        "translated_text": build_text(text, target)
    }));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_huawei_doc_submit(state: &AppState, headers: &HeaderMap) -> Response {
    simple_stat(state, "huawei-doc", headers).await;
    let job_id = next_job_id(state, "huawei-doc");
    store_job(
        state,
        &job_id,
        "huawei-doc",
        json!({ "status": "FINISH", "url": translated_media_url("document") }),
        None,
        None,
        None,
    ).await;
    json_response(json!({ "id": job_id }))
}

async fn handle_huawei_doc_poll(state: &AppState, path: &str) -> Response {
    let job_id = path.rsplit('/').next().unwrap_or_default();
    let Some(job) = load_job(state, job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error_msg": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_tencent_signed(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let timestamp = header_value(headers, "x-tc-timestamp").unwrap_or_default();
    let host = header_value(headers, "host").unwrap_or_default();
    let stat_key = "official:tencent".to_string();
    let _ = key_enter(state, &stat_key).await;
    let params = verify::Tc3Params {
        authorization,
        timestamp,
        body: body.to_string(),
        host,
        path: "/".to_string(),
    };
    if let Err(_error) = verify::verify_tc3(&params) {
        key_exit(state, &stat_key).await;
        return json_response(json!({
            "Response": {
                "Error": {
                    "Code": "AuthFailure.SignatureFailure",
                    "Message": "The provided credentials could not be verified."
                },
                "RequestId": "req-mock-auth-failure"
            }
        }));
    }
    let action = header_value(headers, "x-tc-action").unwrap_or_default();
    let value = parse_json(body);
    if action == "DescribeVideoTranslateJob" {
        let job_id = value.get("JobId").and_then(Value::as_str).unwrap_or_default();
        let Some(mut job) = load_job(state, job_id).await else {
            key_exit(state, &stat_key).await;
            return json_response(json!({
                "Response": {
                    "Error": {
                        "Code": "ResourceNotFound.JobNotFound",
                        "Message": "JobId does not exist."
                    },
                    "RequestId": "req-mock-video-not-found"
                }
            }));
        };
        let count = job.poll_count;
        job.poll_count += 1;
        state.official_jobs.lock().await.insert(job_id.to_string(), job.clone());
        key_exit(state, &stat_key).await;
        if count == 0 {
            return json_response(json!({
                "Response": {
                    "JobId": job_id,
                    "Status": "1",
                    "Progress": 50,
                    "RequestId": next_job_id(state, "tmt")
                }
            }));
        } else {
            return json_response(json!({
                "Response": {
                    "JobId": job_id,
                    "Status": "5",
                    "Progress": 100,
                    "ResultVideoUrl": translated_media_url("video"),
                    "ResultSubtitleUrl": translated_media_url("document"),
                    "RequestId": next_job_id(state, "tmt")
                }
            }));
        }
    }
    let response = match action.as_str() {
        // Official TMT shape: Response carries RequestId + echoed Source /
        // Target language codes alongside TargetText.
        "TextTranslate" => {
            let target = value.get("Target").and_then(Value::as_str).unwrap_or("en");
            let source = value.get("Source").and_then(Value::as_str).unwrap_or("auto");
            if !crate::languages::is_valid_tencent_code(target) || (source != "auto" && !crate::languages::is_valid_tencent_code(source)) {
                json_response(json!({
                    "Response": {
                        "Error": {
                            "Code": "InvalidParameterValue.UnsupportedTargetLanguage",
                            "Message": "Unsupported target language."
                        },
                        "RequestId": "req-mock-lang-failure"
                    }
                }))
            } else {
                json_response(json!({
                    "Response": {
                        "RequestId": next_job_id(state, "tmt"),
                        "Source": source,
                        "Target": target,
                        "TargetText": build_text(value.get("SourceText").and_then(Value::as_str).unwrap_or_default(), target)
                    }
                }))
            }
        }
        "ImageTranslate" | "ImageTranslateLLM" => {
            let target = value.get("Target").and_then(Value::as_str).unwrap_or("en");
            let translated = build_text("image text", target);
            json_response(json!({
                "Response": {
                    "SessionUuid": "mock-session-uuid",
                    "Image": translated_media_url("image"),
                    "Data": translated_media_url("image"),
                    "TargetText": translated,
                    "RequestId": next_job_id(state, "tmt")
                }
            }))
        }
        "FileTranslate" => json_response(json!({ "Response": { "Data": { "TaskId": next_job_id(state, "tencent-doc") }, "RequestId": next_job_id(state, "tmt") } })),
        "ChatTranslations" => json_response(json!({ "Response": { "Choices": [{ "Message": { "Content": build_text(value.get("Text").and_then(Value::as_str).unwrap_or_default(), value.get("Target").and_then(Value::as_str).unwrap_or("en")) } }] } })),
        "SubmitVideoTranslateJob" => {
            let job_id = next_job_id(state, "tencent-video");
            store_job(
                state,
                &job_id,
                "tencent-video",
                json!({ "Response": { "Status": "1" } }),
                None,
                None,
                None,
            ).await;
            json_response(json!({ "Response": { "JobId": job_id, "RequestId": next_job_id(state, "tmt") } }))
        }
        _ => json_response(json!({ "Response": { "RequestId": next_job_id(state, "tmt"), "TargetText": build_text("signed request", "en") } })),
    };
    key_exit(state, &stat_key).await;
    response
}

async fn handle_aws_signed(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let amz_date = header_value(headers, "x-amz-date").unwrap_or_default();
    let host = header_value(headers, "host").unwrap_or_default();
    let stat_key = "official:aws".to_string();
    let _ = key_enter(state, &stat_key).await;
    let params = verify::AwsSigV4Params {
        authorization,
        amz_date,
        body: body.to_string(),
        host,
        path: "/".to_string(),
    };
    if let Err(_error) = verify::verify_aws_sigv4(&params) {
        key_exit(state, &stat_key).await;
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "__type": "UnrecognizedClientException",
                "Message": "The security token included in the request is invalid."
            })),
        )
            .into_response();
    }
    let target = header_value(headers, "x-amz-target").unwrap_or_default();
    let value = parse_json(body);
    let response = if target.ends_with(".TranslateText") {
        let tgt = value.get("TargetLanguageCode").and_then(Value::as_str).unwrap_or("en");
        let src = value.get("SourceLanguageCode").and_then(Value::as_str).unwrap_or("auto");
        if !crate::languages::is_valid_aws_code(tgt) || (src != "auto" && !crate::languages::is_valid_aws_code(src)) {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "__type": "UnsupportedLanguagePairException",
                    "Message": "The language pair was not supported."
                })),
            )
                .into_response()
        } else {
            json_response(json!({
                "TranslatedText": build_text(value.get("Text").and_then(Value::as_str).unwrap_or_default(), tgt),
                "SourceLanguageCode": src,
                "TargetLanguageCode": tgt
            }))
        }
    } else if target.ends_with(".StartTextTranslationJob") {
        let job_id = next_job_id(state, "aws-doc");
        json_response(json!({
            "JobId": job_id,
            "JobStatus": "SUBMITTED"
        }))
    } else if target.ends_with(".DescribeTextTranslationJob") {
        let job_id = value.get("JobId").and_then(Value::as_str).unwrap_or("aws-doc-job");
        json_response(json!({
            "TextTranslationJobProperties": {
                "JobId": job_id,
                "JobStatus": "COMPLETED",
                "SourceLanguageCode": "en",
                "TargetLanguageCodes": ["zh"],
                "OutputDataConfig": {
                    "S3Uri": "s3://mock-bucket/output/"
                }
            }
        }))
    } else if target.ends_with(".TranslateDocument") {
        json_response(json!({ "TranslatedDocument": { "Content": base64_of("document") } }))
    } else if target == "Textract.DetectDocumentText" {
        json_response(json!({ "Blocks": [{ "Text": build_text("ocr text", "en") }] }))
    } else if target == "RekognitionService.DetectText" {
        json_response(json!({ "TextDetections": [{ "DetectedText": build_text("ocr text", "en") }] }))
    } else if headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .contains("bedrock")
    {
        json_response(json!({ "output": { "message": { "content": [{ "text": build_text(body, "en") }] } } }))
    } else {
        json_response(json!({ "TranslatedText": build_text("signed request", "en") }))
    };
    key_exit(state, &stat_key).await;
    response
}

async fn handle_alibaba_signed(state: &AppState, body: &str) -> Response {
    let stat_key = "official:alibaba".to_string();
    let _ = key_enter(state, &stat_key).await;
    let params_vec: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let action = params_vec
        .iter()
        .find(|(key, _)| key == "Action")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let signature = params_vec
        .iter()
        .find(|(key, _)| key == "Signature")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    if let Err(error) = verify::verify_alibaba(&verify::AlibabaParams {
        params: params_vec.clone(),
        signature,
    }) {
        if action != "TranslateImage" {
            key_exit(state, &stat_key).await;
            return auth_error_response(error);
        }
    }
    let params: HashMap<String, String> = params_vec.into_iter().collect();
    let response = match action.as_str() {
        "TranslateGeneral" => json_response(json!({
            "Data": {
                "Translated": build_text(params.get("SourceText").map(String::as_str).unwrap_or_default(), params.get("TargetLanguage").map(String::as_str).unwrap_or("en")),
                "TranslatedText": build_text(params.get("SourceText").map(String::as_str).unwrap_or_default(), params.get("TargetLanguage").map(String::as_str).unwrap_or("en"))
            }
        })),
        "TranslateImage" => json_response(json!({ "Data": { "FinalText": build_text("image text", params.get("TargetLanguage").map(String::as_str).unwrap_or("en")), "FinalImageUrl": translated_media_url("image") } })),
        "CreateDocTranslateTask" => {
            let job_id = next_job_id(state, "ali-doc");
            store_job(
                state,
                &job_id,
                "ali-doc",
                json!({ "Status": "translated", "TranslateFileUrl": translated_media_url("document") }),
                None,
                None,
                None,
            ).await;
            json_response(json!({ "TaskId": job_id }))
        }
        "SubmitVideoTranslationJob" => {
            let job_id = next_job_id(state, "ali-video");
            store_job(
                state,
                &job_id,
                "ali-video",
                json!({
                    "State": "Finished",
                    "JobResult": {
                        "MediaUrl": translated_media_url("video")
                    }
                }),
                None,
                None,
                None,
            ).await;
            json_response(json!({ "JobId": job_id }))
        }
        "GetDocTranslateTask" => {
            let job_id = params.get("TaskId").cloned().unwrap_or_default();
            let Some(job) = load_job(state, &job_id).await else {
                key_exit(state, &stat_key).await;
                return (StatusCode::NOT_FOUND, Json(json!({ "Message": "job not found" }))).into_response();
            };
            json_response(job.poll_response)
        }
        "GetSmartHandleJob" => {
            let job_id = params.get("JobId").cloned().unwrap_or_default();
            let Some(job) = load_job(state, &job_id).await else {
                key_exit(state, &stat_key).await;
                return (StatusCode::NOT_FOUND, Json(json!({ "Message": "job not found" }))).into_response();
            };
            json_response(job.poll_response)
        }
        _ => json_response(json!({ "Data": { "Translated": build_text("alibaba request", "en") } })),
    };
    key_exit(state, &stat_key).await;
    response
}

async fn handle_reverie(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "reverie", headers).await;
    let value = parse_json(body);
    let text = value.get("data").and_then(Value::as_array).and_then(|items| items.first()).and_then(Value::as_str).unwrap_or_default();
    json_response(json!({ "responseList": [build_text(text, "en")] }))
}

async fn handle_pangeanic_text(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "pangeanic-text", headers).await;
    let value = parse_json(body);
    let target = value.get("tgt").and_then(Value::as_str).unwrap_or("en");
    let text = value.get("text").and_then(Value::as_array).and_then(|items| items.first()).and_then(Value::as_str).unwrap_or_default();
    json_response(json!({ "tgt": [build_text(text, target)] }))
}

async fn handle_pangeanic_doc_submit(state: &AppState) -> Response {
    let job_id = next_job_id(state, "pangeanic");
    store_job(
        state,
        &job_id,
        "pangeanic-doc",
        json!({ "processingStatus": "completed" }),
        Some("document"),
        Some("pangeanic-translated.docx".into()),
        Some("application/octet-stream"),
    ).await;
    json_response(json!({ "guid": job_id }))
}

async fn handle_pangeanic_doc_poll(state: &AppState, query: &HashMap<String, String>) -> Response {
    let job_id = query.get("guid").cloned().unwrap_or_default();
    let Some(job) = load_job(state, &job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "job not found" }))).into_response();
    };
    json_response(job.poll_response)
}

async fn handle_pangeanic_doc_download(state: &AppState, query: &HashMap<String, String>) -> Response {
    let job_id = query.get("guid").cloned().unwrap_or_default();
    let Some(job) = load_job(state, &job_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "job not found" }))).into_response();
    };
    translated_binary_response(
        job.download_kind.as_deref().unwrap_or("document"),
        job.download_filename.as_deref(),
        job.download_content_type.as_deref(),
    )
}

async fn handle_reka(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "reka", headers).await;
    let value = parse_json(body);
    let target = value.get("target_language").and_then(Value::as_str).unwrap_or("en");
    json_response(json!({
        "translation": build_text("speech text", target),
        "audio_base64": base64_of("audio")
    }))
}

async fn handle_nlpcloud(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "nlpcloud", headers).await;
    let value = parse_json(body);
    json_response(json!({ "translation_text": build_text(value.get("text").and_then(Value::as_str).unwrap_or_default(), value.get("target").and_then(Value::as_str).unwrap_or("en")) }))
}

async fn handle_fptai(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "fptai", headers).await;
    let value = parse_json(body);
    json_response(json!({ "translated_text": build_text(value.get("text").and_then(Value::as_str).unwrap_or_default(), value.get("target_lang").and_then(Value::as_str).unwrap_or("en")) }))
}

async fn handle_generic_text_json(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    simple_stat(state, "generic-text", headers).await;
    let value = parse_json(body);
    let target = value
        .get("target_lang")
        .or_else(|| value.get("target"))
        .and_then(Value::as_str)
        .unwrap_or("en");
    let text = value
        .get("text")
        .or_else(|| value.get("q"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    json_response(json!({ "translated_text": build_text(text, target) }))
}

async fn simple_stat(state: &AppState, family: &str, headers: &HeaderMap) {
    let key = header_value(headers, "authorization")
        .or_else(|| header_value(headers, "x-api-key"))
        .or_else(|| header_value(headers, "api-key"))
        .unwrap_or_else(|| "none".into());
    let stat_key = format!("official:{family}:{key}");
    let _ = key_enter(state, &stat_key).await;
    key_exit(state, &stat_key).await;
}

async fn simple_azure_stat(state: &AppState, headers: &HeaderMap) {
    let key = header_value(headers, "ocp-apim-subscription-key").unwrap_or_else(|| "none".into());
    let stat_key = format!("official:azure:{key}");
    let _ = key_enter(state, &stat_key).await;
    key_exit(state, &stat_key).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_parser_extracts_text_and_filename() {
        let content_type = "multipart/form-data; boundary=abc123";
        let body = concat!(
            "--abc123\r\n",
            "Content-Disposition: form-data; name=\"target_lang\"\r\n\r\n",
            "zh\r\n",
            "--abc123\r\n",
            "Content-Disposition: form-data; name=\"file\"; filename=\"demo.docx\"\r\n",
            "Content-Type: application/octet-stream\r\n\r\n",
            "binary\r\n",
            "--abc123--\r\n"
        );
        let payload = parse_multipart(content_type, body.as_bytes());
        assert_eq!(payload.fields.get("target_lang").map(String::as_str), Some("zh"));
        assert_eq!(payload.filenames.get("file").map(String::as_str), Some("demo.docx"));
    }

    #[test]
    fn extract_target_lang_from_prompt_text() {
        assert_eq!(
            extract_target_lang("Translate to zh-CN and output only translation."),
            Some("zh-CN".to_string())
        );
    }
}
