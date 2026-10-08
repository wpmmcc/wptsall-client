//! Persistent state-machine translation pipeline: Plan → Fetch → Translate → Sync.
//!
//! This module provides:
//! - Translation helper functions (moved from discoverer.rs for shared use)
//! - `translate_item_fields()`: pure translation logic → builds callback payload
//! - Persistence functions for crash-resilient translation:
//!   - `persist_raw_content()`: raw content → disk, item status pending→fetched
//!   - `persist_translated()`: translated payload → disk, item status fetched→translated
//!   - `sync_item_to_wp()`: read payload from disk → submit callback → item status translated→done
//!
//! # File layout
//! ```text
//! data/
//!   raw/{domain_key}/rel_{relation_id}/{object_type}_{object_id}.json
//!   translated/{domain_key}/rel_{relation_id}/{object_type}_{object_id}.json
//! ```
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::Context;
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use url::Url;

use crate::component_rt::content_safety::{
    is_technical_key, is_technical_string, php_translatable_strings,
};
use crate::component_rt::loader::resolve_runtime_for_content_format;
use crate::component_rt::proxy::ProxyClientPool;
use crate::component_rt::runner::{
    translate_non_text_via_component, translate_non_text_via_component_with_env,
    translate_rich_html_blocks, translate_rich_html_blocks_with_env,
    translate_text_with_constraints, translate_text_with_constraints_with_env,
};
use crate::component_rt::selector::select_component_with_format_awareness;
use crate::db::jobs::{update_item_status, update_item_translated_path};
use crate::db::pending_callbacks::{add_pending_callback, PendingCallbackEntry};
use crate::logging::{log_event, snippet, unix_ts};
use crate::task_engine::submitter::{
    retry_with_backoff, send_i18n_translation_callback, send_translation_callback,
    upload_pending_media_durable,
};
use crate::types::*;

pub(crate) mod language_pack;

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct TranslationBuildTrace {
    pub(crate) payload: TranslationCallbackPayload,
    pub(crate) idempotency_key: String,
    pub(crate) component_ids: Vec<String>,
}

/// Sanitize a WP base URL into a filesystem-safe domain key.
///
/// Examples:
/// - `https://blog.example.com` → `https-blog.example.com`
/// - `http://localhost:8080/wp` → `http-localhost-8080`
pub(crate) fn sanitize_domain_key(wp_base: &str) -> String {
    normalize_domain_base_for_key(wp_base)
        .replace("://", "-")
        .replace(['/', ':'], "-")
        .to_lowercase()
}

fn normalize_domain_base_for_key(wp_base: &str) -> String {
    let trimmed = wp_base.trim();
    if let Ok(parsed) = Url::parse(trimmed) {
        if let Some(host) = parsed.host_str() {
            let scheme = parsed.scheme().to_lowercase();
            let host = host.to_lowercase();
            if let Some(port) = parsed.port() {
                return format!("{}://{}:{}", scheme, host, port);
            }
            return format!("{}://{}", scheme, host);
        }
    }

    // Fallback for unexpected non-URL inputs.
    trimmed.trim_end_matches('/').to_lowercase()
}

fn short_domain_fingerprint(wp_base: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalize_domain_base_for_key(wp_base).as_bytes());
    let digest = hasher.finalize();
    digest[..6].iter().map(|b| format!("{:02x}", b)).collect()
}

/// Normalize path separators to `/`. The pipeline's canonical layout strings
/// use `/`, but on Windows the data dir (and paths built from PathBuf) arrive
/// with `\`; std::fs accepts both on Windows, so comparison is the only place
/// separators matter. Found by the 3-OS CI matrix: the raw-file cleanup never
/// matched on Windows and silently leaked raw files after every sync.
fn normalize_sep(path: &str) -> String {
    path.replace('\\', "/")
}

/// `<data_dir>/<kind>/` prefix in separator-agnostic form.
fn data_dir_kind_prefix(data_dir: &str, kind: &str) -> String {
    let base = normalize_sep(data_dir).trim_end_matches('/').to_string();
    format!("{}/{}/", base, kind)
}

/// Split the `<kind>/<rest...>` suffix off a data-dir path, agnostic to the
/// platform separator. Returns None when the path does not live under the
/// given kind directory.
pub(crate) fn strip_data_dir_kind(path: &str, data_dir: &str, kind: &str) -> Option<String> {
    normalize_sep(path)
        .strip_prefix(&data_dir_kind_prefix(data_dir, kind))
        .map(|rest| rest.to_string())
}

/// Derive the translated file path from a raw path by substituting the
/// `raw/` segment with `translated/`.
pub(crate) fn build_translated_path(raw_path: &str, data_dir: &str, _domain_key: &str) -> String {
    match strip_data_dir_kind(raw_path, data_dir, "raw") {
        Some(rest) => format!("{}{}", data_dir_kind_prefix(data_dir, "translated"), rest),
        None => format!("{}.translated", raw_path),
    }
}

/// CLI mode may not have a DB; create an in-memory one for pipeline tracking.
pub(crate) fn ensure_db(
    db: Option<Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
) -> Arc<tokio::sync::Mutex<rusqlite::Connection>> {
    if let Some(existing) = db {
        return existing;
    }
    let conn = crate::db::open_db(":memory:").expect("in-memory DB for pipeline");
    Arc::new(tokio::sync::Mutex::new(conn))
}

pub(crate) fn derive_business_line_from_rule(
    rule: Option<&DiscoveredRule>,
    object_type: &str,
) -> &'static str {
    if let Some(rule) = rule {
        if rule.source_group.eq_ignore_ascii_case("config_object")
            || rule.routing_profile.eq_ignore_ascii_case("config_i18n")
        {
            return "config_i18n";
        }
    }

    match object_type.trim().to_ascii_lowercase().as_str() {
        "post" | "post_type" => "post_content",
        "term" | "taxonomy" => "taxonomy_content",
        _ => "custom_model",
    }
}

// ---------------------------------------------------------------------------
// Content format helpers (moved from discoverer.rs)
// ---------------------------------------------------------------------------

/// Known content_format values accepted by the runtime pipeline.
/// Canonical source: `libs/wptsall-contracts/content_formats.json`.
pub(crate) const KNOWN_CONTENT_FORMATS: &[&str] =
    crate::component_rt::contract::CANONICAL_CONTENT_FORMATS;

/// Normalize content_format for runtime use.
///
/// Empty values still default to `plain_text` for backward compatibility.
/// Explicit but unknown values are rejected so the pipeline does not silently
/// translate unsupported formats as plain text.
pub(crate) fn get_safe_content_format(
    raw_format: &str,
    field_name: &str,
    log_file: &str,
) -> anyhow::Result<String> {
    let normalized = crate::component_rt::contract::normalize_content_format_alias(raw_format);
    if normalized.is_empty() {
        return Ok("plain_text".to_string());
    }
    if KNOWN_CONTENT_FORMATS.contains(&normalized.as_str()) {
        return Ok(normalized);
    }
    let _ = log_event(
        log_file,
        "warning",
        "discovery.unknown_content_format",
        json!({
            "field": field_name,
            "raw_format": raw_format,
            "blocked": true
        }),
    );
    Err(anyhow::anyhow!(
        "unsupported content_format '{}' for field '{}'",
        raw_format,
        field_name
    ))
}

/// Safety guard for stripping nested translation markers.
///
/// Protects against pathological content where markers are nested too deeply
/// (e.g. repeated self-translation), which could otherwise overflow stack.
const MAX_MARKER_STRIP_DEPTH: usize = 256;

/// Safety guard for nested JSON translation recursion.
///
/// Deeply nested JSON payloads fail before unsafe whole-string translation.
const MAX_JSON_TRANSLATION_DEPTH: usize = 256;

/// Strip outer translation markers 【lang】...【/lang】 from text.
///
/// Prevents double-wrapping when source content was already translated in a prior run.
/// Example: `【en_US】Hello【/en_US】` → `Hello`
pub(crate) fn strip_translation_markers(text: &str) -> String {
    let mut current = text.trim();
    let mut changed = false;
    let open_len = '【'.len_utf8();
    let close_len = '】'.len_utf8();

    for _ in 0..MAX_MARKER_STRIP_DEPTH {
        if !current.starts_with('【') {
            break;
        }

        let Some(rel_pos) = current[open_len..].find('】') else {
            break;
        };
        let close_pos = open_len + rel_pos;
        let lang_code = &current[open_len..close_pos];
        if lang_code.is_empty()
            || !lang_code
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            break;
        }

        let closing = format!("【/{}】", lang_code);
        if !current.ends_with(closing.as_str()) {
            break;
        }

        let content_start = close_pos + close_len;
        let content_end = current.len() - closing.len();
        if content_start > content_end {
            break;
        }

        current = &current[content_start..content_end];
        changed = true;
    }

    if changed {
        current.to_string()
    } else {
        text.to_string()
    }
}

/// Translate a field value according to its content_format.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn translate_field_value(
    client: &Client,
    comp: &ComponentRuntime,
    text: &str,
    field_name: &str,
    content_format: &str,
    source_lang: &str,
    target_lang: &str,
    constraints: &EffectiveConstraints,
    log_file: &str,
    object_id: i64,
) -> anyhow::Result<Option<String>> {
    translate_field_value_with_env(
        client,
        comp,
        text,
        field_name,
        content_format,
        source_lang,
        target_lang,
        constraints,
        log_file,
        object_id,
        None,
    )
    .await
}

/// GAP-04 (批 I 2026-09-23): field translation with a durable async-job
/// env. The env identifies the translation unit (field of this object);
/// chunking widens it per chunk, structured formats per leaf. Sync
/// components ignore it entirely (the runner filters it).
pub(crate) async fn translate_field_value_with_env(
    client: &Client,
    comp: &ComponentRuntime,
    text: &str,
    field_name: &str,
    content_format: &str,
    source_lang: &str,
    target_lang: &str,
    constraints: &EffectiveConstraints,
    log_file: &str,
    object_id: i64,
    async_env: Option<crate::db::async_jobs::AsyncJobEnv>,
) -> anyhow::Result<Option<String>> {
    match content_format {
        "code" => Ok(None),

        "media_ref" => {
            // Media metadata text in media_ref still uses text translation, even
            // though the original field keeps media_ref write-back semantics.
            if is_media_text_field(field_name) {
                let translated = translate_text_with_constraints_with_env(
                    client,
                    comp,
                    text,
                    source_lang,
                    target_lang,
                    constraints,
                    async_env,
                )
                .await?;
                Ok(Some(translated))
            } else {
                Ok(None)
            }
        }

        "plain_text" | "slug" => {
            let translated = translate_text_with_constraints_with_env(
                client,
                comp,
                text,
                source_lang,
                target_lang,
                constraints,
                async_env,
            )
            .await?;
            Ok(Some(translated))
        }

        "rich_html" => {
            let translated = translate_rich_html_blocks_with_env(
                client,
                comp,
                text,
                source_lang,
                target_lang,
                constraints,
                async_env,
            )
            .await?;
            Ok(Some(translated))
        }

        "serialized_php" => translate_serialized_php_with_env(
            client,
            comp,
            text,
            source_lang,
            target_lang,
            constraints,
            log_file,
            async_env,
        )
        .await
        .map(Some)
        .with_context(|| {
            format!(
                "serialized_php translation failed for field '{field_name}' (object={object_id})"
            )
        }),

        "json_structured" => translate_json_structured_with_env(
            client,
            comp,
            text,
            source_lang,
            target_lang,
            constraints,
            log_file,
            async_env,
        )
        .await
        .map(Some)
        .with_context(|| {
            format!(
                "json_structured translation failed for field '{field_name}' (object={object_id})"
            )
        }),

        _ => Err(anyhow::anyhow!(
            "unsupported content_format '{}' for field '{}'",
            content_format,
            field_name
        )),
    }
}

/// Translate PHP serialized string: parse, translate string values, re-serialize.
pub(crate) async fn translate_serialized_php(
    client: &Client,
    comp: &ComponentRuntime,
    text: &str,
    source_lang: &str,
    target_lang: &str,
    constraints: &EffectiveConstraints,
    log_file: &str,
) -> anyhow::Result<String> {
    translate_serialized_php_with_env(
        client,
        comp,
        text,
        source_lang,
        target_lang,
        constraints,
        log_file,
        None,
    )
    .await
}

/// GAP-04: serialized-php translation with durable async-job env. Each
/// translated `s:N:"..."` string gets its own unit identity via a stable
/// sequential leaf index (`php_s:<n>`) — stable because the re-offered
/// source text is byte-identical.
pub(crate) async fn translate_serialized_php_with_env(
    client: &Client,
    comp: &ComponentRuntime,
    text: &str,
    source_lang: &str,
    target_lang: &str,
    constraints: &EffectiveConstraints,
    _log_file: &str,
    async_env: Option<crate::db::async_jobs::AsyncJobEnv>,
) -> anyhow::Result<String> {
    // Validate the whole source before any billable leaf request.
    let strings = php_translatable_strings(text)?;
    let mut result = String::with_capacity(text.len());
    let mut pos = 0;
    for leaf in strings {
        let translated = translate_text_with_constraints_with_env(
            client,
            comp,
            leaf.value,
            source_lang,
            target_lang,
            constraints,
            async_env
                .as_ref()
                .map(|env| env.with_sub_path(&format!("php_s:{}", leaf.ordinal))),
        )
        .await?;
        result.push_str(&text[pos..leaf.start]);
        result.push_str(&format!("s:{}:\"{}\";", translated.len(), translated));
        pos = leaf.end;
    }
    result.push_str(&text[pos..]);
    Ok(result)
}

/// Translate JSON structured content: parse JSON, translate string values,
/// preserve structure (keys, numbers, booleans, nulls).
pub(crate) async fn translate_json_structured(
    client: &Client,
    comp: &ComponentRuntime,
    text: &str,
    source_lang: &str,
    target_lang: &str,
    constraints: &EffectiveConstraints,
    log_file: &str,
) -> anyhow::Result<String> {
    translate_json_structured_with_env(
        client,
        comp,
        text,
        source_lang,
        target_lang,
        constraints,
        log_file,
        None,
    )
    .await
}

/// GAP-04: JSON-structured translation with durable async-job env. Leaf
/// strings get path-based unit identities (`$.a[0].b`) so each leaf's
/// provider job is independently resumable.
pub(crate) async fn translate_json_structured_with_env(
    client: &Client,
    comp: &ComponentRuntime,
    text: &str,
    source_lang: &str,
    target_lang: &str,
    constraints: &EffectiveConstraints,
    _log_file: &str,
    async_env: Option<crate::db::async_jobs::AsyncJobEnv>,
) -> anyhow::Result<String> {
    let parsed: Value = serde_json::from_str(text)
        .with_context(|| "failed to parse JSON for json_structured translation")?;

    let translated = translate_json_value_with_env(
        client,
        comp,
        &parsed,
        source_lang,
        target_lang,
        constraints,
        async_env.as_ref(),
        "$",
    )
    .await?;

    serde_json::to_string(&translated).with_context(|| "failed to re-serialize translated JSON")
}

/// Recursively translate string values in a JSON Value, preserving structure.
pub(crate) fn translate_json_value<'a>(
    client: &'a Client,
    comp: &'a ComponentRuntime,
    value: &'a Value,
    source_lang: &'a str,
    target_lang: &'a str,
    constraints: &'a EffectiveConstraints,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send + 'a>> {
    translate_json_value_with_depth(
        client,
        comp,
        value,
        source_lang,
        target_lang,
        constraints,
        0,
    )
}

/// GAP-04 env-aware variant of [`translate_json_value`] (path-tracked).
pub(crate) fn translate_json_value_with_env<'a>(
    client: &'a Client,
    comp: &'a ComponentRuntime,
    value: &'a Value,
    source_lang: &'a str,
    target_lang: &'a str,
    constraints: &'a EffectiveConstraints,
    async_env: Option<&'a crate::db::async_jobs::AsyncJobEnv>,
    path: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send + 'a>> {
    translate_json_value_with_depth_env(
        client,
        comp,
        value,
        source_lang,
        target_lang,
        constraints,
        async_env,
        path,
        0,
    )
}

fn translate_json_value_with_depth<'a>(
    client: &'a Client,
    comp: &'a ComponentRuntime,
    value: &'a Value,
    source_lang: &'a str,
    target_lang: &'a str,
    constraints: &'a EffectiveConstraints,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send + 'a>> {
    translate_json_value_with_depth_env(
        client,
        comp,
        value,
        source_lang,
        target_lang,
        constraints,
        None,
        "$",
        depth,
    )
}

#[allow(clippy::too_many_arguments)]
fn translate_json_value_with_depth_env<'a>(
    client: &'a Client,
    comp: &'a ComponentRuntime,
    value: &'a Value,
    source_lang: &'a str,
    target_lang: &'a str,
    constraints: &'a EffectiveConstraints,
    async_env: Option<&'a crate::db::async_jobs::AsyncJobEnv>,
    path: &'a str,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send + 'a>> {
    Box::pin(async move {
        if depth > MAX_JSON_TRANSLATION_DEPTH {
            return Err(anyhow::anyhow!(
                "json_structured nesting too deep (>{})",
                MAX_JSON_TRANSLATION_DEPTH
            ));
        }

        match value {
            Value::String(s) => {
                if is_technical_string(s) {
                    return Ok(value.clone());
                }
                let translated = translate_text_with_constraints_with_env(
                    client,
                    comp,
                    s,
                    source_lang,
                    target_lang,
                    constraints,
                    async_env.map(|env| env.with_sub_path(path)),
                )
                .await?;
                Ok(Value::String(translated))
            }
            Value::Array(arr) => {
                let mut translated_arr = Vec::with_capacity(arr.len());
                for (idx, item) in arr.iter().enumerate() {
                    let child_path = format!("{path}[{idx}]");
                    translated_arr.push(
                        translate_json_value_with_depth_env(
                            client,
                            comp,
                            item,
                            source_lang,
                            target_lang,
                            constraints,
                            async_env,
                            &child_path,
                            depth + 1,
                        )
                        .await?,
                    );
                }
                Ok(Value::Array(translated_arr))
            }
            Value::Object(obj) => {
                let mut translated_obj = serde_json::Map::with_capacity(obj.len());
                for (key, val) in obj {
                    if is_technical_key(key) {
                        translated_obj.insert(key.clone(), val.clone());
                        continue;
                    }
                    let child_path = format!("{path}.{}", key);
                    translated_obj.insert(
                        key.clone(),
                        translate_json_value_with_depth_env(
                            client,
                            comp,
                            val,
                            source_lang,
                            target_lang,
                            constraints,
                            async_env,
                            &child_path,
                            depth + 1,
                        )
                        .await?,
                    );
                }
                Ok(Value::Object(translated_obj))
            }
            _ => Ok(value.clone()),
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldRestorePlan {
    None,
    JsonToSerializedPhp,
}

#[derive(Debug, Clone)]
struct NormalizedFieldInput {
    text: String,
    effective_format: String,
    restore_plan: FieldRestorePlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldTranslationKind {
    Text,
    MediaAsset,
}

#[derive(Debug, Clone)]
struct FieldFormatAdapter {
    source_content_format: String,
    execution_content_format: String,
    routing_content_format: String,
    preferred_task_type: String,
    routing_group_key: String,
    translation_kind: FieldTranslationKind,
}

#[derive(Debug, Clone)]
struct PreparedFieldTranslation {
    field_name: String,
    normalized: NormalizedFieldInput,
    adapter: FieldFormatAdapter,
    storage: String,
    raw_value: Value,
}

fn task_type_for_content_format(content_format: &str) -> &'static str {
    match content_format {
        // media_ref groups should route to media-capable component first.
        "media_ref" => "image",
        _ => "text",
    }
}

fn build_field_format_adapter(
    field_name: &str,
    source_content_format: &str,
    normalized: &NormalizedFieldInput,
    raw_value: &Value,
    complete_data: &serde_json::Map<String, Value>,
) -> FieldFormatAdapter {
    if normalized.effective_format == "media_ref" && is_media_text_field(field_name) {
        // Media metadata text follows the text component path, but the field
        // still keeps media_ref source semantics for write-back and auditing.
        return FieldFormatAdapter {
            source_content_format: source_content_format.to_string(),
            execution_content_format: "plain_text".to_string(),
            routing_content_format: "plain_text".to_string(),
            preferred_task_type: "text".to_string(),
            routing_group_key: "plain_text".to_string(),
            translation_kind: FieldTranslationKind::Text,
        };
    }

    if normalized.effective_format == "media_ref" {
        let media_task_type =
            extract_media_reference_for_field(field_name, raw_value, complete_data)
                .map(|(_source_id, source_ref)| {
                    infer_media_task_type_from_ref(&source_ref, "image")
                })
                .unwrap_or_else(|| "image".to_string());
        return FieldFormatAdapter {
            source_content_format: source_content_format.to_string(),
            execution_content_format: "media_ref".to_string(),
            routing_content_format: "media_ref".to_string(),
            routing_group_key: format!("media_ref::{}", media_task_type),
            preferred_task_type: media_task_type,
            translation_kind: FieldTranslationKind::MediaAsset,
        };
    }

    if normalized.effective_format == "slug" {
        // Slug translation reuses the plain-text component lane. The WP write-back
        // side is responsible for the final URL-safe sanitize + uniqueness check.
        return FieldFormatAdapter {
            source_content_format: source_content_format.to_string(),
            execution_content_format: "plain_text".to_string(),
            routing_content_format: "plain_text".to_string(),
            preferred_task_type: "text".to_string(),
            routing_group_key: "plain_text".to_string(),
            translation_kind: FieldTranslationKind::Text,
        };
    }

    let routing_content_format = normalized.effective_format.clone();
    FieldFormatAdapter {
        source_content_format: source_content_format.to_string(),
        execution_content_format: routing_content_format.clone(),
        preferred_task_type: task_type_for_content_format(&routing_content_format).to_string(),
        routing_group_key: routing_content_format.clone(),
        routing_content_format,
        translation_kind: FieldTranslationKind::Text,
    }
}

fn media_artifact_hints_for_task_type(
    task_type: &str,
) -> (Option<&'static str>, Option<&'static str>) {
    match task_type {
        "image" => (Some("image_file"), Some("translated_image_file")),
        "audio" => (Some("audio_file"), Some("translated_audio_file")),
        "video" => (Some("video_file"), Some("translated_video_file")),
        "document" => (Some("document_file"), Some("translated_document_file")),
        _ => (None, None),
    }
}

fn is_media_text_field(field_name: &str) -> bool {
    let lower = field_name.to_lowercase();
    lower.contains("alt")
        || lower.contains("caption")
        || lower.contains("title")
        || lower.contains("description")
        || lower.contains("excerpt")
        || lower == "post_content"
}

fn looks_like_http_url(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("http://") || trimmed.starts_with("https://")
}

fn get_string_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

fn get_u64_from_value(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn extract_field_capability_string(
    rule: Option<&DiscoveredRule>,
    field_name: &str,
    key: &str,
) -> Option<String> {
    let caps = rule?.field_capabilities.as_object()?;
    let field_cfg = caps.get(field_name)?.as_object()?;
    field_cfg
        .get(key)
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

fn resolve_field_content_format(rule: Option<&DiscoveredRule>, field_name: &str) -> Option<String> {
    if let Some(rule) = rule {
        if let Some(value) = rule.field_content_formats.get(field_name) {
            return Some(value.clone());
        }
    }
    extract_field_capability_string(rule, field_name, "content_format")
}

fn resolve_field_storage(rule: Option<&DiscoveredRule>, field_name: &str) -> Option<String> {
    if let Some(rule) = rule {
        if let Some(value) = rule.field_storage_map.get(field_name) {
            return Some(value.clone());
        }
    }
    extract_field_capability_string(rule, field_name, "storage")
}

fn get_nested_field_value<'a>(
    complete_data: &'a serde_json::Map<String, Value>,
    container_key: &str,
    field_name: &str,
) -> Option<&'a Value> {
    complete_data
        .get(container_key)
        .and_then(|value| value.as_object())
        .and_then(|object| object.get(field_name))
}

fn get_authoritative_field_value<'a>(
    complete_data: &'a serde_json::Map<String, Value>,
    field_name: &str,
    storage: &str,
) -> Option<&'a Value> {
    match storage {
        "post_column" => get_nested_field_value(complete_data, "post", field_name)
            .or_else(|| complete_data.get(field_name)),
        "term_column" => get_nested_field_value(complete_data, "term", field_name)
            .or_else(|| complete_data.get(field_name)),
        "option_value" => get_nested_field_value(complete_data, "option", field_name)
            .or_else(|| complete_data.get(field_name)),
        "post_meta" | "term_meta" | "meta" => {
            get_nested_field_value(complete_data, "meta", field_name)
                .or_else(|| complete_data.get(field_name))
        }
        _ => complete_data.get(field_name),
    }
}

fn infer_media_task_type_from_ref(source_ref: &str, fallback: &str) -> String {
    let Some(ext) = extract_file_extension_from_ref(source_ref) else {
        return fallback.to_string();
    };

    const IMAGE_EXT: &[&str] = &[
        "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "svg", "avif",
    ];
    const VIDEO_EXT: &[&str] = &["mp4", "mov", "avi", "webm", "mkv", "m4v"];
    const AUDIO_EXT: &[&str] = &["mp3", "wav", "ogg", "flac", "aac", "m4a"];
    const DOC_EXT: &[&str] = &[
        "pdf",
        "doc",
        "docx",
        "ppt",
        "pptx",
        "xls",
        "xlsx",
        "txt",
        "csv",
        "html",
        "htm",
        "json",
        "yaml",
        "yml",
        "xml",
        "properties",
        "strings",
        "po",
        "pot",
        "srt",
        "vtt",
        "rtf",
        "tex",
        "odt",
    ];

    if IMAGE_EXT.contains(&ext.as_str()) {
        return "image".to_string();
    }
    if VIDEO_EXT.contains(&ext.as_str()) {
        return "video".to_string();
    }
    if AUDIO_EXT.contains(&ext.as_str()) {
        return "audio".to_string();
    }
    if DOC_EXT.contains(&ext.as_str()) {
        return "document".to_string();
    }
    fallback.to_string()
}

fn extract_file_extension_from_ref(source_ref: &str) -> Option<String> {
    let ref_lc = source_ref.trim().to_lowercase();
    if ref_lc.is_empty() {
        return None;
    }
    let no_query = ref_lc
        .split('?')
        .next()
        .unwrap_or(ref_lc.as_str())
        .split('#')
        .next()
        .unwrap_or(ref_lc.as_str());
    let ext = no_query.rsplit('.').next().unwrap_or_default().trim();
    if ext.is_empty() || ext == no_query {
        return None;
    }
    if ext.len() > 12 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext.to_string())
}

fn infer_storage_fallback(object_type: &str, field_name: &str) -> String {
    if object_type == "option" {
        let _ = field_name;
        return "option_value".to_string();
    }

    let is_tax = matches!(object_type, "term" | "taxonomy");
    if is_tax {
        const TERM_COLUMNS: &[&str] = &["term_id", "name", "slug", "description", "parent"];
        if TERM_COLUMNS.contains(&field_name) {
            return "term_column".to_string();
        }
        return "term_meta".to_string();
    }

    const POST_COLUMNS: &[&str] = &[
        "ID",
        "post_title",
        "post_content",
        "post_excerpt",
        "post_date",
        "post_status",
        "post_type",
        "post_name",
        "post_author",
        "post_parent",
        "comment_status",
        "ping_status",
        "menu_order",
        "guid",
    ];
    if POST_COLUMNS.contains(&field_name) {
        return "post_column".to_string();
    }
    "post_meta".to_string()
}

fn extract_media_reference_for_field(
    field_name: &str,
    raw_value: &Value,
    complete_data: &serde_json::Map<String, Value>,
) -> Option<(u64, String)> {
    let related_string = |key: &str| {
        complete_data
            .get(key)
            .and_then(get_string_from_value)
            .or_else(|| {
                get_nested_field_value(complete_data, "meta", key).and_then(get_string_from_value)
            })
            .or_else(|| {
                get_nested_field_value(complete_data, "post", key).and_then(get_string_from_value)
            })
            .or_else(|| {
                get_nested_field_value(complete_data, "term", key).and_then(get_string_from_value)
            })
    };
    let related_u64 = |key: &str| {
        complete_data
            .get(key)
            .and_then(get_u64_from_value)
            .or_else(|| {
                get_nested_field_value(complete_data, "meta", key).and_then(get_u64_from_value)
            })
            .or_else(|| {
                get_nested_field_value(complete_data, "post", key).and_then(get_u64_from_value)
            })
            .or_else(|| {
                get_nested_field_value(complete_data, "term", key).and_then(get_u64_from_value)
            })
    };

    let mut source_id: u64 = 0;
    let mut source_ref = String::new();

    if let Some(id) = get_u64_from_value(raw_value) {
        source_id = id;
    }

    if let Some(str_val) = get_string_from_value(raw_value) {
        if looks_like_http_url(&str_val) {
            source_ref = str_val;
        } else if source_id == 0 {
            source_id = str_val.parse::<u64>().unwrap_or(0);
        }
    }

    if let Some(obj) = raw_value.as_object() {
        if source_id == 0 {
            for key in ["source_id", "attachment_id", "id", "media_id"] {
                if let Some(v) = obj.get(key).and_then(get_u64_from_value) {
                    source_id = v;
                    break;
                }
            }
        }
        if source_ref.is_empty() {
            for key in ["source_ref", "source_url", "url", "src", "ref"] {
                if let Some(v) = obj.get(key).and_then(get_string_from_value) {
                    source_ref = v;
                    break;
                }
            }
        }
    }

    if source_ref.is_empty() && field_name.ends_with("_id") {
        let key = format!("{}_url", field_name.trim_end_matches("_id"));
        if let Some(v) = related_string(&key) {
            source_ref = v;
        }
    }
    if source_ref.is_empty() {
        let key = format!("{}_url", field_name);
        if let Some(v) = related_string(&key) {
            source_ref = v;
        }
    }
    if source_id == 0 && field_name.ends_with("_url") {
        let key = format!("{}_id", field_name.trim_end_matches("_url"));
        if let Some(v) = related_u64(&key) {
            source_id = v;
        }
    }

    if source_id == 0 && source_ref.is_empty() {
        None
    } else {
        Some((source_id, source_ref))
    }
}

fn infer_group_media_file_extension(
    fields_in_group: &[PreparedFieldTranslation],
    complete_data: &serde_json::Map<String, Value>,
) -> Option<String> {
    let mut ext: Option<String> = None;
    for prepared in fields_in_group {
        if prepared.adapter.translation_kind != FieldTranslationKind::MediaAsset {
            continue;
        }
        let Some((_source_id, source_ref)) = extract_media_reference_for_field(
            &prepared.field_name,
            &prepared.raw_value,
            complete_data,
        ) else {
            continue;
        };
        let Some(current) = extract_file_extension_from_ref(&source_ref) else {
            continue;
        };
        if let Some(existing) = ext.as_ref() {
            if existing != &current {
                // Mixed extensions in one group: disable extension filtering.
                return None;
            }
        } else {
            ext = Some(current);
        }
    }
    ext
}

fn canonicalize_json_for_hash(value: &Value) -> Value {
    match value {
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize_json_for_hash).collect()),
        Value::Object(obj) => {
            let mut keys: Vec<String> = obj.keys().cloned().collect();
            keys.sort();
            let mut out = serde_json::Map::with_capacity(obj.len());
            for key in keys {
                // FL-13 (SIM-15): client-internal plumbing keys (single
                // underscore prefix, e.g. `_wptsall_outbox_id` — merged into
                // complete_data by the outbox lane) are NOT source content.
                // Hashing them split the idempotency key per lane: the same
                // object+content derived one key from the outbox row and a
                // DIFFERENT one from the /content listing, so cross-lane
                // retries could never hit the site's idempotency cache.
                // Site-served keys (including the double-underscore
                // `__wptsall_job_snapshot`) stay in the hash.
                if key.starts_with("_wptsall_") {
                    continue;
                }
                if let Some(v) = obj.get(&key) {
                    out.insert(key, canonicalize_json_for_hash(v));
                }
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

fn compute_object_snapshot_hash(value: &Value) -> String {
    let canonical = canonicalize_json_for_hash(value);
    let serialized = match serde_json::to_vec(&canonical) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let mut hasher = Sha256::new();
    hasher.update(&serialized);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

fn build_callback_attempt_id(worker_id: &str, relation_id: i64, object_id: i64) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("att-{}-{}-{}-{}", worker_id, relation_id, object_id, now_ms)
}

#[derive(Debug, Clone, Default)]
struct FieldResultMeta {
    provider_component: String,
    merge_target: String,
    transform_stage: String,
    fallback_reason: String,
}

fn fallback_reason_from_detail(detail: &str) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let candidate = trimmed.split(':').next().unwrap_or_default().trim();
    if candidate.is_empty() {
        return String::new();
    }
    if candidate
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return candidate.to_string();
    }
    String::new()
}

fn merge_target_for_storage(storage: &str) -> &'static str {
    if matches!(storage, "post_meta" | "term_meta" | "meta") {
        "translated_meta"
    } else {
        "translated_fields"
    }
}

fn field_is_protected_name(field_name: &str, rule: Option<&DiscoveredRule>) -> bool {
    const BLOCKED_PREFIXES: &[&str] = &["_wptsall_", "_wp_", "_edit_", "_oembed_"];
    let allowed_internal_field = field_name == "_wp_attachment_image_alt";
    let explicit_rule_allows_field = resolve_field_content_format(rule, field_name).is_some()
        || resolve_field_storage(rule, field_name).is_some();
    BLOCKED_PREFIXES
        .iter()
        .any(|prefix| field_name.starts_with(prefix))
        && !allowed_internal_field
        && !explicit_rule_allows_field
}

fn patch_string_for_copy(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(serde_json::to_string(other).unwrap_or_default()),
    }
}

/// Copy ships the source bytes with no provider call. Skip records the
/// decision and writes nothing, including no empty string.
fn apply_copy_and_skip_fields(
    rule: Option<&DiscoveredRule>,
    item: &ContentItem,
    complete_data: &serde_json::Map<String, Value>,
    copy_fields: &[String],
    skip_fields: &[String],
    fields_failed: &mut u32,
    translated_fields: &mut serde_json::Map<String, Value>,
    translated_meta: &mut serde_json::Map<String, Value>,
    field_results: &mut Vec<CallbackFieldResult>,
) {
    for field_name in skip_fields {
        push_field_result(
            field_results,
            field_name,
            "skipped",
            "",
            "",
            "user_excluded",
        );
    }
    for field_name in copy_fields {
        if field_is_protected_name(field_name, rule) {
            push_field_result(
                field_results,
                field_name,
                "skipped",
                "",
                "",
                "blocked_prefix",
            );
            continue;
        }
        let storage = if let Some(storage) = resolve_field_storage(rule, field_name) {
            storage
        } else if rule.is_some_and(|candidate| !candidate.field_storage_map.is_empty()) {
            *fields_failed += 1;
            push_field_result(
                field_results,
                field_name,
                "failed",
                "plain_text",
                "",
                "missing_storage_mapping",
            );
            continue;
        } else {
            infer_storage_fallback(item.object_type.as_str(), field_name)
        };
        let Some(raw) = get_authoritative_field_value(complete_data, field_name, &storage) else {
            push_field_result(
                field_results,
                field_name,
                "skipped",
                "plain_text",
                &storage,
                "empty_or_null",
            );
            continue;
        };
        let Some(text) = patch_string_for_copy(raw) else {
            push_field_result(
                field_results,
                field_name,
                "skipped",
                "plain_text",
                &storage,
                "empty_or_null",
            );
            continue;
        };
        let meta = FieldResultMeta {
            provider_component: String::new(),
            merge_target: merge_target_for_storage(&storage).to_string(),
            transform_stage: "copied_without_provider".to_string(),
            fallback_reason: String::new(),
        };
        if matches!(storage.as_str(), "post_meta" | "term_meta" | "meta") {
            translated_meta.insert(field_name.clone(), Value::String(text));
        } else {
            translated_fields.insert(field_name.clone(), Value::String(text));
        }
        push_field_result_with_meta(
            field_results,
            field_name,
            "success",
            "plain_text",
            &storage,
            "copied_without_provider",
            &meta,
        );
    }
}

fn transform_stage_for_prepared(prepared: &PreparedFieldTranslation) -> String {
    if prepared.adapter.source_content_format == "media_ref"
        && prepared.adapter.execution_content_format == "plain_text"
    {
        return "media_text_to_plain_text".to_string();
    }
    if prepared.adapter.source_content_format == "serialized_php"
        && prepared.adapter.execution_content_format == "json_structured"
        && prepared.normalized.restore_plan == FieldRestorePlan::JsonToSerializedPhp
    {
        return "serialized_php_to_json_structured".to_string();
    }
    if prepared.adapter.translation_kind == FieldTranslationKind::MediaAsset {
        return "media_ref_direct".to_string();
    }
    if prepared.adapter.source_content_format != prepared.adapter.execution_content_format {
        return "format_adapted".to_string();
    }
    "direct".to_string()
}

fn field_result_meta_for_prepared(
    prepared: &PreparedFieldTranslation,
    provider_component: &str,
    merge_target: &str,
) -> FieldResultMeta {
    FieldResultMeta {
        provider_component: provider_component.to_string(),
        merge_target: merge_target.to_string(),
        transform_stage: transform_stage_for_prepared(prepared),
        fallback_reason: String::new(),
    }
}

fn push_field_result(
    field_results: &mut Vec<CallbackFieldResult>,
    field: &str,
    status: &str,
    content_format: &str,
    storage: &str,
    detail: &str,
) {
    let meta = FieldResultMeta {
        fallback_reason: fallback_reason_from_detail(detail),
        ..FieldResultMeta::default()
    };
    push_field_result_with_meta(
        field_results,
        field,
        status,
        content_format,
        storage,
        detail,
        &meta,
    );
}

fn push_field_result_with_meta(
    field_results: &mut Vec<CallbackFieldResult>,
    field: &str,
    status: &str,
    content_format: &str,
    storage: &str,
    detail: &str,
    meta: &FieldResultMeta,
) {
    field_results.push(CallbackFieldResult {
        field: field.to_string(),
        status: status.to_string(),
        content_format: content_format.to_string(),
        storage: storage.to_string(),
        detail: detail.to_string(),
        provider_component: meta.provider_component.clone(),
        merge_target: meta.merge_target.clone(),
        transform_stage: meta.transform_stage.clone(),
        fallback_reason: if meta.fallback_reason.is_empty() {
            fallback_reason_from_detail(detail)
        } else {
            meta.fallback_reason.clone()
        },
    });
}

/// Serialize a JSON value to PHP serialized string form.
///
/// This is used when WP already expanded a serialized meta value into
/// array/object in `complete_data`; after translation we re-encode it so
/// WP merge/write-back keeps the original storage contract.
fn serialize_json_value_to_php(value: &Value) -> String {
    match value {
        Value::Null => "N;".to_string(),
        Value::Bool(v) => format!("b:{};", if *v { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                format!("i:{};", i)
            } else if let Some(u) = n.as_u64() {
                if i64::try_from(u).is_ok() {
                    format!("i:{};", u)
                } else {
                    format!("d:{};", u)
                }
            } else if let Some(f) = n.as_f64() {
                format!("d:{};", f)
            } else {
                "i:0;".to_string()
            }
        }
        Value::String(s) => format!("s:{}:\"{}\";", s.len(), s),
        Value::Array(arr) => {
            let mut out = String::new();
            out.push_str(&format!("a:{}:{{", arr.len()));
            for (idx, item) in arr.iter().enumerate() {
                out.push_str(&format!("i:{};", idx));
                out.push_str(&serialize_json_value_to_php(item));
            }
            out.push('}');
            out
        }
        Value::Object(obj) => {
            let mut out = String::new();
            out.push_str(&format!("a:{}:{{", obj.len()));
            for (key, val) in obj {
                out.push_str(&format!("s:{}:\"{}\";", key.len(), key));
                out.push_str(&serialize_json_value_to_php(val));
            }
            out.push('}');
            out
        }
    }
}

fn normalize_field_value_for_translation(
    value: &Value,
    content_format: &str,
    field_name: &str,
    object_id: i64,
    log_file: &str,
) -> anyhow::Result<Option<NormalizedFieldInput>> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => {
            if s.is_empty() {
                return Ok(None);
            }
            Ok(Some(NormalizedFieldInput {
                text: s.clone(),
                effective_format: content_format.to_string(),
                restore_plan: FieldRestorePlan::None,
            }))
        }
        non_string => {
            let raw_json = serde_json::to_string(non_string)
                .with_context(|| "failed to serialize non-string field value")?;
            let (effective_format, restore_plan) = if content_format == "serialized_php" {
                // Use JSON path to translate values (not keys), then restore to
                // PHP serialized to preserve downstream storage semantics.
                (
                    "json_structured".to_string(),
                    FieldRestorePlan::JsonToSerializedPhp,
                )
            } else {
                (content_format.to_string(), FieldRestorePlan::None)
            };
            let _ = log_event(
                log_file,
                "info",
                "discovery.non_string_field_normalized",
                json!({
                    "field": field_name,
                    "object_id": object_id,
                    "content_format": content_format,
                    "effective_format": effective_format,
                    "value_type": match non_string {
                        Value::Array(_) => "array",
                        Value::Object(_) => "object",
                        Value::Number(_) => "number",
                        Value::Bool(_) => "bool",
                        _ => "other"
                    }
                }),
            );
            Ok(Some(NormalizedFieldInput {
                text: raw_json,
                effective_format,
                restore_plan,
            }))
        }
    }
}

fn restore_translated_value_after_translation(
    translated: String,
    restore_plan: FieldRestorePlan,
    field_name: &str,
    object_id: i64,
    log_file: &str,
) -> String {
    match restore_plan {
        FieldRestorePlan::None => translated,
        FieldRestorePlan::JsonToSerializedPhp => match serde_json::from_str::<Value>(&translated) {
            Ok(v) => serialize_json_value_to_php(&v),
            Err(err) => {
                let _ = log_event(
                    log_file,
                    "warning",
                    "discovery.serialized_php_reencode_failed",
                    json!({
                        "field": field_name,
                        "object_id": object_id,
                        "error": snippet(&format!("{:#}", err)),
                        "fallback": "keep_json_string"
                    }),
                );
                translated
            }
        },
    }
}

/// Extract translatable field names from field_capabilities.
pub(crate) fn extract_translate_fields(field_caps: &Value) -> Vec<String> {
    let mut fields = Vec::new();

    // Format 1: explicit translate_fields array inside field_capabilities
    if let Some(translate) = field_caps
        .get("translate_fields")
        .and_then(|v| v.as_array())
    {
        for f in translate {
            if let Some(name) = f.as_str() {
                fields.push(name.to_string());
            }
        }
    }

    // Format 2 & 3: per-field action map (fallback if format 1 is empty)
    if fields.is_empty() {
        if let Some(obj) = field_caps.as_object() {
            for (key, val) in obj {
                if val.as_str() == Some("translate")
                    || (val.get("type").and_then(|t| t.as_str()) == Some("translate")
                        && val.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true))
                {
                    fields.push(key.clone());
                }
            }
        }
    }

    fields
}

fn resolve_plugin_slug_for_rule_binding(
    relation: &DiscoveredRelation,
    rule: Option<&DiscoveredRule>,
) -> Option<String> {
    if let Some(r) = rule {
        if let Some(model) = relation
            .models
            .iter()
            .find(|m| m.model_id == r.model_id && !m.plugin_slug.trim().is_empty())
        {
            return Some(model.plugin_slug.trim().to_lowercase());
        }
    }

    if relation.models.len() == 1 {
        if let Some(model) = relation.models.first() {
            let slug = model.plugin_slug.trim();
            if !slug.is_empty() {
                return Some(slug.to_lowercase());
            }
        }
    }

    let template = relation.template.trim();
    if template.is_empty() {
        None
    } else {
        Some(template.to_lowercase())
    }
}

fn translate_fields_for_rule(rule: &DiscoveredRule) -> Vec<String> {
    super::field_action::plan_field_actions(rule).translate
}

fn rule_present_field_score(
    item: &ContentItem,
    complete_data: &serde_json::Map<String, Value>,
    rule: &DiscoveredRule,
) -> usize {
    let plan = super::field_action::plan_field_actions(rule);
    plan.translate
        .iter()
        .chain(plan.copy.iter())
        .filter(|field_name| {
            let storage = if let Some(storage) = resolve_field_storage(Some(rule), field_name) {
                storage
            } else if !rule.field_storage_map.is_empty() {
                return false;
            } else {
                infer_storage_fallback(item.object_type.as_str(), field_name)
            };
            get_authoritative_field_value(complete_data, field_name, &storage).is_some()
        })
        .count()
}

fn select_rule_for_item<'a>(
    item: &ContentItem,
    complete_data: &serde_json::Map<String, Value>,
    rules: &'a [DiscoveredRule],
) -> Option<&'a DiscoveredRule> {
    let expected_data_type = match item.object_type.as_str() {
        "post" | "post_type" => Some("post"),
        "term" | "taxonomy" => Some("term"),
        "option" => Some("option"),
        _ => None,
    };
    let data_type_matches = |rule: &DiscoveredRule| {
        let normalized_data_type = match rule.data_type.as_str() {
            "post_type" => "post",
            "taxonomy" => "term",
            other => other,
        };
        rule.object_name == item.subtype
            && expected_data_type
                .map(|dt| normalized_data_type.is_empty() || normalized_data_type == dt)
                .unwrap_or(true)
    };

    let mut candidates = rules
        .iter()
        .filter(|rule| data_type_matches(rule))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates = rules
            .iter()
            .filter(|rule| rule.object_name == item.subtype)
            .collect::<Vec<_>>();
    }

    let mut selected = None;
    let mut selected_score = 0usize;
    for candidate in candidates {
        let score = rule_present_field_score(item, complete_data, candidate);
        if selected.is_none() || score > selected_score {
            selected = Some(candidate);
            selected_score = score;
        }
    }
    selected
}

/// Build the no-provider attachment lifecycle lane.
///
/// Attachment binary changes must still reach write-back when a site has no
/// user-facing text rule for `attachment`.  The Client copies the source file
/// from the configured WordPress origin, uploads it through the authenticated
/// media endpoint, and lets the normal callback/write-back path record the
/// relation-scoped mapping.  Attachment metadata is still translated by the
/// ordinary rule pipeline when a rule is configured.
fn build_attachment_copy_trace(
    wp_base: &str,
    item: &ContentItem,
    relation: &DiscoveredRelation,
    worker_config: &WorkerConfig,
    complete_data: &serde_json::Map<String, Value>,
) -> anyhow::Result<Option<TranslationBuildTrace>> {
    if item.subtype != "attachment" || item.object_id <= 0 {
        return Ok(None);
    }

    let source_ref = complete_data
        .get("attachment_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(source_ref) = source_ref else {
        return Ok(None);
    };

    let source_revision = item
        .complete_data
        .get("__wptsall_job_snapshot")
        .and_then(|snapshot| snapshot.get("source_revision"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if source_revision.is_empty() {
        eprintln!(
            "skipping callback: missing source_revision job snapshot (object_id={}, object_type={})",
            item.object_id, "post_type"
        );
        return Err(anyhow::anyhow!(
            "missing source_revision for object {} ({})",
            item.object_id,
            "post_type"
        ));
    }

    let object_snapshot_hash = compute_object_snapshot_hash(&item.complete_data);
    let snapshot_fp = if object_snapshot_hash.is_empty() {
        "nohash"
    } else {
        &object_snapshot_hash[..std::cmp::min(12, object_snapshot_hash.len())]
    };
    let domain_fp = short_domain_fingerprint(wp_base);
    let idempotency_key = format!(
        "discovery-{}-{}-post_type-{}-{}",
        domain_fp, relation.id, item.object_id, snapshot_fp
    );

    Ok(Some(TranslationBuildTrace {
        payload: TranslationCallbackPayload {
            schema_version: crate::config::TASK_CALLBACK_SCHEMA_VERSION,
            attempt_id: build_callback_attempt_id(
                &worker_config.worker_id,
                relation.id,
                item.object_id,
            ),
            object_snapshot_hash,
            source_revision,
            policy_version: item
                .complete_data
                .get("__wptsall_job_snapshot")
                .and_then(|snapshot| snapshot.get("policy_version"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            field_results: vec![CallbackFieldResult {
                field: "attachment_binary".to_string(),
                status: "success".to_string(),
                content_format: "media_ref".to_string(),
                storage: "media_mappings".to_string(),
                detail: "attachment_source_copy".to_string(),
                provider_component: String::new(),
                merge_target: "media_mappings".to_string(),
                transform_stage: "authenticated_source_copy".to_string(),
                fallback_reason: String::new(),
            }],
            relation_id: u64::try_from(relation.id).unwrap_or(0),
            business_line: "post_content".to_string(),
            object_type: "post_type".to_string(),
            subtype: item.subtype.clone(),
            object_id: u64::try_from(item.object_id).unwrap_or(0),
            translated_fields: HashMap::new(),
            translated_meta: HashMap::new(),
            media_mappings: vec![MediaMapping {
                source_id: u64::try_from(item.object_id).unwrap_or(0),
                translated_ref: source_ref.to_string(),
                attachment_id: None,
                source_copy: true,
            }],
            media_field_sources: HashMap::new(),
            client_task_id: idempotency_key.clone(),
            outbox_id: item
                .complete_data
                .get("_wptsall_outbox_id")
                .and_then(Value::as_u64),
            worker_id: worker_config.worker_id.clone(),
            source_lang: relation.source_lang.clone(),
            target_lang: relation.target_lang.clone(),
            execution_time_ms: 0,
        },
        idempotency_key,
        component_ids: Vec::new(),
    }))
}

/// FSE content is stored as core post types and therefore commonly has no
/// relation translation rule. Build a conservative rule in the Client so
/// Site Editor saves still enter the normal component/callback pipeline.
/// Gutenberg templates, parts and navigation use rich_html (the block-aware
/// runner preserves top-level block boundaries); global styles are JSON.
fn build_fse_synthetic_rule(item: &ContentItem) -> Option<DiscoveredRule> {
    if item.object_type != "post_type"
        || !matches!(
            item.subtype.as_str(),
            "wp_template" | "wp_template_part" | "wp_navigation" | "wp_global_styles"
        )
    {
        return None;
    }

    let content_format = if item.subtype == "wp_global_styles" {
        "json_structured"
    } else {
        "rich_html"
    };
    Some(DiscoveredRule {
        id: 0,
        model_id: 0,
        name: format!("FSE synthetic rule ({})", item.subtype),
        data_type: "post".to_string(),
        object_name: item.subtype.clone(),
        field_capabilities: json!({
            "post_title": { "type": "translate", "enabled": true, "storage": "post_column", "content_format": "plain_text" },
            "post_content": { "type": "translate", "enabled": true, "storage": "post_column", "content_format": content_format }
        }),
        translate_fields: vec!["post_title".to_string(), "post_content".to_string()],
        related_taxonomies: Vec::new(),
        field_content_formats: HashMap::from([
            ("post_title".to_string(), "plain_text".to_string()),
            ("post_content".to_string(), content_format.to_string()),
        ]),
        field_storage_map: HashMap::from([
            ("post_title".to_string(), "post_column".to_string()),
            ("post_content".to_string(), "post_column".to_string()),
        ]),
        source_group: "fse_core".to_string(),
        routing_profile: "fse_block_content".to_string(),
        delivery_target: "wp_post_type".to_string(),
        required_component_slots: Vec::new(),
        required_content_formats: vec!["plain_text".to_string(), content_format.to_string()],
        field_source_roles: HashMap::new(),
    })
}

// ---------------------------------------------------------------------------
// translate_item_fields — pure translation logic
// ---------------------------------------------------------------------------

/// Pure translation: ContentItem + rules + component → TranslationCallbackPayload.
///
/// Does NOT submit callbacks or manage pending state. Returns `None` if there
/// are no translatable fields. The caller is responsible for persisting the
/// result and submitting the callback.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn translate_item_fields_with_trace_using_proxy(
    client: &Client,
    proxy_pool: Option<&ProxyClientPool>,
    wp_base: &str,
    item: &ContentItem,
    relation: &DiscoveredRelation,
    rules: &[DiscoveredRule],
    component_registry: Option<&ComponentRuntimeRegistry>,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
    rule_component_bindings: Option<&RuleComponentBindingsDoc>,
    worker_config: &WorkerConfig,
    log_file: &str,
    claimed_by_other_relations: Option<&std::collections::HashSet<String>>,
    async_scope: Option<crate::db::async_jobs::AsyncJobScope>,
) -> anyhow::Result<Option<TranslationBuildTrace>> {
    let item_start = std::time::Instant::now();
    let complete_data = item.complete_data.as_object().cloned().unwrap_or_default();

    // Find the matching rule for this content item.
    // Prefer subtype + data_type match, then choose the candidate whose
    // declared translatable fields are actually present on this object.
    let configured_rule = select_rule_for_item(item, &complete_data, rules);
    let synthetic_rule = if configured_rule.is_none() {
        build_fse_synthetic_rule(item)
    } else {
        None
    };
    let rule = configured_rule.or(synthetic_rule.as_ref());
    let plugin_slug_for_binding = resolve_plugin_slug_for_rule_binding(relation, rule);

    // Extract translatable fields
    let action_plan = rule
        .map(super::field_action::plan_field_actions)
        .unwrap_or_default();
    let translate_fields = action_plan.translate.clone();
    let copy_fields = action_plan.copy.clone();
    let skip_fields = action_plan.skip.clone();

    if translate_fields.is_empty() && copy_fields.is_empty() {
        match build_attachment_copy_trace(wp_base, item, relation, worker_config, &complete_data)? {
            Some(mut trace) => {
                bind_manual_callback(&mut trace, async_scope.as_ref())?;
                return Ok(Some(trace));
            }
            None => {}
        }
        let _ = log_event(
            log_file,
            "warning",
            "discovery.no_translatable_fields",
            json!({
                "relation_id": relation.id,
                "object_id": item.object_id,
                "object_type": item.object_type,
                "subtype": item.subtype,
                "matched_rule_id": rule.map(|r| r.id),
                "matched_rule_object_name": rule.map(|r| r.object_name.clone()),
                "matched_rule_data_type": rule.map(|r| r.data_type.clone()),
                "rules_total": rules.len(),
            }),
        );
        return Ok(None);
    }

    // Determine business_line from object_type
    let business_line = derive_business_line_from_rule(rule, &item.object_type);

    let field_storage_map = rule
        .map(|r| &r.field_storage_map)
        .cloned()
        .unwrap_or_default();

    let mut translated_fields = serde_json::Map::new();
    let mut translated_meta = serde_json::Map::new();
    let mut media_mappings_by_source: HashMap<u64, MediaMapping> = HashMap::new();
    let mut media_field_sources: HashMap<String, u64> = HashMap::new();
    let mut field_results: Vec<CallbackFieldResult> = Vec::new();
    let mut fields_failed = 0u32;
    let mut prepared_by_format: BTreeMap<String, Vec<PreparedFieldTranslation>> = BTreeMap::new();
    let mut used_component_ids: BTreeSet<String> = BTreeSet::new();

    for field_name in &translate_fields {
        const BLOCKED_PREFIXES: &[&str] = &["_wptsall_", "_wp_", "_edit_", "_oembed_"];
        let allowed_internal_field = field_name == "_wp_attachment_image_alt";
        let explicit_rule_allows_field = resolve_field_content_format(rule, field_name).is_some()
            || resolve_field_storage(rule, field_name).is_some();
        if BLOCKED_PREFIXES
            .iter()
            .any(|pfx| field_name.starts_with(pfx))
            && !allowed_internal_field
            && !explicit_rule_allows_field
        {
            push_field_result(
                &mut field_results,
                field_name,
                "skipped",
                "",
                "",
                "blocked_prefix",
            );
            continue;
        }
        let storage = if let Some(storage) = resolve_field_storage(rule, field_name) {
            storage
        } else if !field_storage_map.is_empty() {
            fields_failed += 1;
            push_field_result(
                &mut field_results,
                field_name,
                "failed",
                resolve_field_content_format(rule, field_name)
                    .as_deref()
                    .unwrap_or("plain_text"),
                "",
                "missing_storage_mapping",
            );
            let _ = log_event(
                log_file,
                "warning",
                "discovery.missing_storage_mapping",
                json!({
                    "field": field_name,
                    "object_id": item.object_id,
                    "object_type": item.object_type,
                    "subtype": item.subtype,
                    "rule_id": rule.map(|r| r.id),
                }),
            );
            continue;
        } else {
            infer_storage_fallback(item.object_type.as_str(), field_name)
        };

        if let Some(value) = get_authoritative_field_value(&complete_data, field_name, &storage) {
            let raw_format = resolve_field_content_format(rule, field_name)
                .unwrap_or_else(|| "plain_text".to_string());
            let content_format = match get_safe_content_format(&raw_format, field_name, log_file) {
                Ok(value) => value,
                Err(err) => {
                    fields_failed += 1;
                    push_field_result(
                        &mut field_results,
                        field_name,
                        "failed",
                        &raw_format,
                        &storage,
                        "unsupported_content_format",
                    );
                    let _ = log_event(
                        log_file,
                        "warning",
                        "discovery.unsupported_content_format",
                        json!({
                            "field": field_name,
                            "object_id": item.object_id,
                            "raw_format": raw_format,
                            "error": snippet(&format!("{:#}", err))
                        }),
                    );
                    continue;
                }
            };

            // Global styles are theme.json-like configuration, not prose.
            // Preserve the JSON byte-for-byte so color tokens, CSS values,
            // presets and references can never be sent to a text provider.
            // The synthetic rule still exposes the field to the callback so
            // the target receives the complete styles object.
            if item.subtype == "wp_global_styles" && field_name == "post_content" {
                if let Some(raw) = value.as_str() {
                    translated_fields.insert(field_name.clone(), Value::String(raw.to_string()));
                    let meta = FieldResultMeta {
                        provider_component: String::new(),
                        merge_target: "translated_fields".to_string(),
                        transform_stage: "fse_global_styles_structure_preserved".to_string(),
                        fallback_reason: String::new(),
                    };
                    push_field_result_with_meta(
                        &mut field_results,
                        field_name,
                        "success",
                        &content_format,
                        &storage,
                        "structure_preserved",
                        &meta,
                    );
                    continue;
                }
            }

            let normalized = match normalize_field_value_for_translation(
                value,
                &content_format,
                field_name,
                item.object_id,
                log_file,
            ) {
                Ok(Some(v)) => v,
                Ok(None) => {
                    push_field_result(
                        &mut field_results,
                        field_name,
                        "skipped",
                        &content_format,
                        "",
                        "empty_or_null",
                    );
                    continue;
                }
                Err(err) => {
                    fields_failed += 1;
                    push_field_result(
                        &mut field_results,
                        field_name,
                        "failed",
                        &content_format,
                        "",
                        &format!("normalization_failed: {}", snippet(&format!("{:#}", err))),
                    );
                    let _ = log_event(
                        log_file,
                        "warning",
                        "discovery.field_normalization_failed",
                        json!({
                            "field": field_name,
                            "object_id": item.object_id,
                            "content_format": content_format,
                            "error": snippet(&format!("{:#}", err))
                        }),
                    );
                    continue;
                }
            };
            if content_format == "code" {
                let meta = FieldResultMeta {
                    provider_component: String::new(),
                    merge_target: merge_target_for_storage(&storage).to_string(),
                    transform_stage: "direct".to_string(),
                    fallback_reason: String::new(),
                };
                push_field_result_with_meta(
                    &mut field_results,
                    field_name,
                    "skipped",
                    &content_format,
                    &storage,
                    "non_translatable_or_no_change",
                    &meta,
                );
                continue;
            }
            let adapter = build_field_format_adapter(
                field_name,
                &content_format,
                &normalized,
                value,
                &complete_data,
            );
            prepared_by_format
                .entry(adapter.routing_group_key.clone())
                .or_default()
                .push(PreparedFieldTranslation {
                    field_name: field_name.clone(),
                    normalized,
                    adapter,
                    storage,
                    raw_value: value.clone(),
                });
        } else {
            let raw_format = resolve_field_content_format(rule, field_name)
                .unwrap_or_else(|| "plain_text".to_string());
            let content_format = match get_safe_content_format(&raw_format, field_name, log_file) {
                Ok(value) => value,
                Err(err) => {
                    fields_failed += 1;
                    push_field_result(
                        &mut field_results,
                        field_name,
                        "failed",
                        &raw_format,
                        &storage,
                        "unsupported_content_format",
                    );
                    let _ = log_event(
                        log_file,
                        "warning",
                        "discovery.unsupported_content_format",
                        json!({
                            "field": field_name,
                            "object_id": item.object_id,
                            "raw_format": raw_format,
                            "error": snippet(&format!("{:#}", err))
                        }),
                    );
                    continue;
                }
            };
            push_field_result(
                &mut field_results,
                field_name,
                "skipped",
                &content_format,
                "",
                "missing_in_complete_data",
            );
        }
    }

    // Select component per content_format group (ISS-09), instead of using one
    // representative field for the whole item.
    let rule_id_for_binding = rule.map(|r| u64::try_from(r.id).unwrap_or(0));
    let relation_id_for_binding = u64::try_from(relation.id).ok();
    let grouped_formats: Vec<String> = prepared_by_format.keys().cloned().collect();
    let total_groups = prepared_by_format.len();
    let mut groups_without_component = 0usize;

    for (group_key, fields_in_group) in prepared_by_format {
        let Some(group_plan) = fields_in_group
            .first()
            .map(|prepared| prepared.adapter.clone())
        else {
            continue;
        };
        let content_format = group_plan.routing_content_format.clone();
        let preferred_task_type = group_plan.preferred_task_type.clone();
        let mut selection_payload = json!({});
        if content_format == "media_ref" {
            if let Some(ext) = infer_group_media_file_extension(&fields_in_group, &complete_data) {
                selection_payload["__file_ext"] = Value::String(ext);
            }
            let (input_artifact_kind, output_artifact_kind) =
                media_artifact_hints_for_task_type(preferred_task_type.as_str());
            if let Some(value) = input_artifact_kind {
                selection_payload["__input_artifact_kind"] = Value::String(value.to_string());
            }
            if let Some(value) = output_artifact_kind {
                selection_payload["__expected_output_artifact_kind"] =
                    Value::String(value.to_string());
            }
        }
        let mut selected_task_type = preferred_task_type.clone();
        let mut component = select_component_with_format_awareness(
            component_registry,
            rule_component_bindings,
            rule_id_for_binding,
            relation_id_for_binding,
            plugin_slug_for_binding.as_deref(),
            &selection_payload,
            business_line,
            preferred_task_type.as_str(),
            &content_format,
            component_id_override,
            component_prefer_ids,
            task_type_component_bindings,
            claimed_by_other_relations,
        );
        if component.is_none() && preferred_task_type != "text" {
            component = select_component_with_format_awareness(
                component_registry,
                rule_component_bindings,
                rule_id_for_binding,
                relation_id_for_binding,
                plugin_slug_for_binding.as_deref(),
                &selection_payload,
                business_line,
                "text",
                &content_format,
                component_id_override,
                component_prefer_ids,
                task_type_component_bindings,
                claimed_by_other_relations,
            );
            if component.is_some() {
                selected_task_type = "text".to_string();
                let _ = log_event(
                    log_file,
                    "info",
                    "discovery.component_task_type_fallback",
                    json!({
                        "relation_id": relation.id,
                        "object_id": item.object_id,
                        "business_line": business_line,
                        "content_format": content_format,
                        "preferred_task_type": preferred_task_type,
                        "fallback_task_type": "text"
                    }),
                );
            }
        }

        let comp = match component {
            Some(c) => c,
            None => {
                groups_without_component += 1;
                fields_failed += fields_in_group.len() as u32;
                for prepared in &fields_in_group {
                    push_field_result(
                        &mut field_results,
                        &prepared.field_name,
                        "failed",
                        &prepared.normalized.effective_format,
                        &prepared.storage,
                        "no_component_for_format",
                    );
                }
                let _ = log_event(
                    log_file,
                    "warning",
                    "discovery.no_component_for_format",
                    json!({
                        "relation_id": relation.id,
                        "object_id": item.object_id,
                        "business_line": business_line,
                        "task_type": preferred_task_type,
                        "content_format": content_format,
                        "group_key": group_key,
                        "fields": fields_in_group.iter().map(|f| f.field_name.clone()).collect::<Vec<_>>()
                    }),
                );
                continue;
            }
        };

        if !comp.supported_content_formats.is_empty()
            && !comp
                .supported_content_formats
                .iter()
                .any(|f| f == &content_format)
        {
            let _ = log_event(
                log_file,
                "warn",
                "discovery.content_format_mismatch",
                json!({
                    "component_id": comp.template.id,
                    "content_format": content_format,
                    "supported": comp.supported_content_formats,
                }),
            );
        }

        let resolved_comp = resolve_runtime_for_content_format(comp, &content_format);
        let component_client = match proxy_pool {
            Some(pool) => pool.get_client(resolved_comp.proxy_profile_id.as_deref())?,
            None => {
                anyhow::ensure!(
                    resolved_comp.proxy_profile_id.is_none(),
                    "configured proxy pool is unavailable; direct fallback refused"
                );
                client
            }
        };
        used_component_ids.insert(resolved_comp.template.id.clone());
        let constraints = EffectiveConstraints::resolve(
            resolved_comp.template.constraints.as_ref(),
            None,
            None,
            worker_config.default_max_input_chars,
            &worker_config.default_split_strategy,
        );

        for prepared in fields_in_group {
            // P8 component circuit: a component whose recent requests keep
            // failing (bad credential / dead endpoint) is suppressed for ALL
            // objects, so a large untranslated backlog cannot burn one
            // provider failure per object while identity cooldowns arm.
            let comp_backoff =
                crate::task_engine::backoff::check_component(&resolved_comp.template.id);
            if comp_backoff.blocked {
                fields_failed += 1;
                push_field_result(
                    &mut field_results,
                    &prepared.field_name,
                    "failed",
                    &prepared.normalized.effective_format,
                    &prepared.storage,
                    &format!(
                        "component_backoff: suppressed {}s after {} consecutive component failures{}",
                        comp_backoff.remaining_secs,
                        comp_backoff.consecutive_failures,
                        if comp_backoff.circuit_open { " (circuit open)" } else { "" }
                    ),
                );
                let _ = log_event(
                    log_file,
                    "warning",
                    "discovery.component_backoff_skip",
                    json!({
                        "component_id": resolved_comp.template.id,
                        "relation_id": relation.id,
                        "object_id": item.object_id,
                        "field": prepared.field_name,
                        "remaining_secs": comp_backoff.remaining_secs,
                        "consecutive_failures": comp_backoff.consecutive_failures,
                        "circuit_open": comp_backoff.circuit_open,
                    }),
                );
                continue;
            }
            let field_name = prepared.field_name.clone();
            let field_content_format = prepared.adapter.source_content_format.clone();
            let field_storage = prepared.storage.clone();
            let is_non_text_media_ref =
                prepared.adapter.translation_kind == FieldTranslationKind::MediaAsset;
            let mut result_meta = field_result_meta_for_prepared(
                &prepared,
                &resolved_comp.template.id,
                merge_target_for_storage(&field_storage),
            );

            if is_non_text_media_ref {
                let maybe_source = extract_media_reference_for_field(
                    &prepared.field_name,
                    &prepared.raw_value,
                    &complete_data,
                );
                let (source_id, source_ref) = match maybe_source {
                    Some(v) => v,
                    None => {
                        fields_failed += 1;
                        result_meta.merge_target = "media_mappings".to_string();
                        push_field_result_with_meta(
                            &mut field_results,
                            &field_name,
                            "failed",
                            &field_content_format,
                            &field_storage,
                            "media_ref_source_missing",
                            &result_meta,
                        );
                        let _ = log_event(
                            log_file,
                            "warning",
                            "discovery.media_ref_source_missing",
                            json!({
                                "field": prepared.field_name,
                                "object_id": item.object_id,
                            }),
                        );
                        continue;
                    }
                };

                let media_task_type =
                    infer_media_task_type_from_ref(&source_ref, selected_task_type.as_str());
                // GAP-04: non-text lane (video/document) — the 06 doc's
                // named GAP-04 cases run through here.
                let media_async_env = async_scope
                    .as_ref()
                    .map(|scope| scope.field_env(&prepared.field_name, "non_text"));
                let media_result = translate_non_text_via_component_with_env(
                    component_client,
                    &resolved_comp,
                    "",
                    Some(&prepared.raw_value),
                    &source_ref,
                    &media_task_type,
                    &prepared.field_name,
                    &relation.source_lang,
                    &relation.target_lang,
                    media_async_env,
                )
                .await;

                match media_result {
                    Ok(outcome) => {
                        crate::task_engine::backoff::record_component_success(
                            &resolved_comp.template.id,
                        );
                        if outcome.translated_ref.trim().is_empty() {
                            fields_failed += 1;
                            result_meta.merge_target = "media_mappings".to_string();
                            push_field_result_with_meta(
                                &mut field_results,
                                &field_name,
                                "failed",
                                &field_content_format,
                                &field_storage,
                                "media_ref_translate_missing_ref",
                                &result_meta,
                            );
                            let _ = log_event(
                                log_file,
                                "warning",
                                "discovery.media_ref_translate_missing_ref",
                                json!({
                                    "field": prepared.field_name,
                                    "object_id": item.object_id,
                                    "source_id": source_id,
                                    "task_type": media_task_type,
                                }),
                            );
                            continue;
                        }

                        let (class_input, class_output) = rule
                            .map(|candidate| {
                                super::content_class::field_class_tokens(
                                    &candidate.field_capabilities,
                                    &field_name,
                                )
                            })
                            .unwrap_or((None, None));
                        let class_decision = super::content_class::decide(
                            &field_content_format,
                            class_input.as_deref(),
                            class_output.as_deref(),
                        );
                        if class_decision.text_only {
                            let Some(text) = super::content_class::text_product(
                                &outcome.translated_text,
                                &outcome.translated_ref,
                            ) else {
                                fields_failed += 1;
                                push_field_result_with_meta(
                                    &mut field_results,
                                    &field_name,
                                    "failed",
                                    &field_content_format,
                                    &field_storage,
                                    "text_output_missing",
                                    &result_meta,
                                );
                                continue;
                            };
                            result_meta.merge_target =
                                merge_target_for_storage(&field_storage).to_string();
                            result_meta.transform_stage = class_decision.detail.to_string();
                            let text = text.to_string();
                            if matches!(field_storage.as_str(), "post_meta" | "term_meta" | "meta") {
                                translated_meta.insert(field_name.clone(), Value::String(text));
                            } else {
                                translated_fields.insert(field_name.clone(), Value::String(text));
                            }
                            push_field_result_with_meta(
                                &mut field_results,
                                &field_name,
                                "success",
                                &field_content_format,
                                &field_storage,
                                class_decision.detail,
                                &result_meta,
                            );
                            continue;
                        }

                        if source_id > 0 {
                            result_meta.merge_target = "media_mappings".to_string();
                            media_mappings_by_source.insert(
                                source_id,
                                MediaMapping {
                                    source_id,
                                    translated_ref: outcome.translated_ref,
                                    attachment_id: None,
                                    source_copy: false,
                                },
                            );
                            media_field_sources.insert(field_name.clone(), source_id);
                            push_field_result_with_meta(
                                &mut field_results,
                                &field_name,
                                "success",
                                &field_content_format,
                                &field_storage,
                                "media_mapping_created",
                                &result_meta,
                            );
                        } else {
                            // URL-only media_ref fields cannot participate in source-id-based
                            // mapping, so write translated URL directly to the destination slot.
                            result_meta.merge_target =
                                merge_target_for_storage(&field_storage).to_string();
                            if matches!(field_storage.as_str(), "post_meta" | "term_meta" | "meta")
                            {
                                translated_meta.insert(
                                    field_name.clone(),
                                    Value::String(outcome.translated_ref),
                                );
                            } else {
                                translated_fields.insert(
                                    field_name.clone(),
                                    Value::String(outcome.translated_ref),
                                );
                            }
                            push_field_result_with_meta(
                                &mut field_results,
                                &field_name,
                                "success",
                                &field_content_format,
                                &field_storage,
                                "media_ref_url_translated",
                                &result_meta,
                            );
                        }
                    }
                    Err(err) => {
                        fields_failed += 1;
                        let comp_decision = crate::task_engine::backoff::record_component_failure(
                            &resolved_comp.template.id,
                        );
                        if comp_decision.circuit_open {
                            let _ = log_event(
                                log_file,
                                "warning",
                                "discovery.component_circuit_open",
                                json!({
                                    "component_id": resolved_comp.template.id,
                                    "relation_id": relation.id,
                                    "object_id": item.object_id,
                                    "field": prepared.field_name,
                                    "consecutive_failures": comp_decision.consecutive_failures,
                                    "cooldown_secs": comp_decision.remaining_secs,
                                }),
                            );
                        }
                        result_meta.merge_target = "media_mappings".to_string();
                        push_field_result_with_meta(
                            &mut field_results,
                            &field_name,
                            "failed",
                            &field_content_format,
                            &field_storage,
                            &format!(
                                "media_ref_translate_failed: {}",
                                snippet(&format!("{:#}", err))
                            ),
                            &result_meta,
                        );
                        let _ = log_event(
                            log_file,
                            "warning",
                            "discovery.media_ref_translate_failed",
                            json!({
                                "field": prepared.field_name,
                                "object_id": item.object_id,
                                "source_id": source_id,
                                "task_type": media_task_type,
                                "error": snippet(&format!("{:#}", err))
                            }),
                        );
                    }
                }
                continue;
            }

            let stripped = strip_translation_markers(&prepared.normalized.text);
            let text_to_translate = stripped.as_str();

            // GAP-04: text lane — field-level env; chunking/structured
            // widen it per chunk / leaf.
            let field_async_env = async_scope
                .as_ref()
                .map(|scope| scope.field_env(&field_name, "text"));
            let translated_result = translate_field_value_with_env(
                component_client,
                &resolved_comp,
                text_to_translate,
                &prepared.field_name,
                &prepared.adapter.execution_content_format,
                &relation.source_lang,
                &relation.target_lang,
                &constraints,
                log_file,
                item.object_id,
                field_async_env,
            )
            .await;

            match translated_result {
                Ok(Some(translated)) => {
                    crate::task_engine::backoff::record_component_success(
                        &resolved_comp.template.id,
                    );
                    let finalized = restore_translated_value_after_translation(
                        translated,
                        prepared.normalized.restore_plan,
                        &prepared.field_name,
                        item.object_id,
                        log_file,
                    );
                    if matches!(
                        prepared.storage.as_str(),
                        "post_meta" | "term_meta" | "meta"
                    ) {
                        translated_meta.insert(field_name.clone(), Value::String(finalized));
                    } else {
                        translated_fields.insert(field_name.clone(), Value::String(finalized));
                    }
                    result_meta.merge_target = merge_target_for_storage(&field_storage).to_string();
                    push_field_result_with_meta(
                        &mut field_results,
                        &field_name,
                        "success",
                        &field_content_format,
                        &field_storage,
                        "translated",
                        &result_meta,
                    );
                }
                Ok(None) => {
                    push_field_result_with_meta(
                        &mut field_results,
                        &field_name,
                        "skipped",
                        &field_content_format,
                        &field_storage,
                        "non_translatable_or_no_change",
                        &result_meta,
                    );
                }
                Err(err) => {
                    fields_failed += 1;
                    let comp_decision = crate::task_engine::backoff::record_component_failure(
                        &resolved_comp.template.id,
                    );
                    if comp_decision.circuit_open {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "discovery.component_circuit_open",
                            json!({
                                "component_id": resolved_comp.template.id,
                                "relation_id": relation.id,
                                "object_id": item.object_id,
                                "field": prepared.field_name,
                                "consecutive_failures": comp_decision.consecutive_failures,
                                "cooldown_secs": comp_decision.remaining_secs,
                            }),
                        );
                    }
                    push_field_result_with_meta(
                        &mut field_results,
                        &field_name,
                        "failed",
                        &field_content_format,
                        &field_storage,
                        &format!("field_translate_failed: {}", snippet(&format!("{:#}", err))),
                        &result_meta,
                    );
                    let _ = log_event(
                        log_file,
                        "warning",
                        "discovery.field_translate_failed",
                        json!({
                            "field": prepared.field_name,
                            "object_id": item.object_id,
                            "source_content_format": prepared.adapter.source_content_format,
                            "execution_content_format": prepared.adapter.execution_content_format,
                            "error": snippet(&format!("{:#}", err))
                        }),
                    );
                }
            }
        }
    }

    apply_copy_and_skip_fields(
        rule,
        item,
        &complete_data,
        &copy_fields,
        &skip_fields,
        &mut fields_failed,
        &mut translated_fields,
        &mut translated_meta,
        &mut field_results,
    );

    if total_groups > 0
        && groups_without_component == total_groups
        && translated_fields.is_empty()
        && translated_meta.is_empty()
        && media_mappings_by_source.is_empty()
    {
        let _ = log_event(
            log_file,
            "warning",
            "discovery.no_component_available",
            json!({
                "relation_id": relation.id,
                "object_id": item.object_id,
                "business_line": business_line,
                "task_type": "grouped",
                "content_formats": grouped_formats,
            }),
        );
        return Err(anyhow::anyhow!(
            "no component runtime available for business_line={}, formats={}",
            business_line,
            grouped_formats.join(",")
        ));
    }

    if translated_fields.is_empty()
        && translated_meta.is_empty()
        && media_mappings_by_source.is_empty()
    {
        let complete_data_keys = complete_data.keys().cloned().collect::<Vec<_>>();
        let meta_keys = complete_data
            .get("meta")
            .and_then(|value| value.as_object())
            .map(|meta| meta.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let field_outcomes = field_results
            .iter()
            .map(|row| {
                json!({
                    "field": row.field,
                    "status": row.status,
                    "content_format": row.content_format,
                    "storage": row.storage,
                    "detail": row.detail,
                    "fallback_reason": row.fallback_reason,
                    "merge_target": row.merge_target,
                })
            })
            .collect::<Vec<_>>();
        let _ = log_event(
            log_file,
            "warning",
            "discovery.no_translation_output",
            json!({
                "object_id": item.object_id,
                "object_type": item.object_type,
                "subtype": item.subtype,
                "relation_id": relation.id,
                "rule_id": rule.map(|r| r.id),
                "rule_object_name": rule.map(|r| r.object_name.clone()),
                "rule_data_type": rule.map(|r| r.data_type.clone()),
                "translate_fields": translate_fields,
                "complete_data_keys": complete_data_keys,
                "meta_keys": meta_keys,
                "fields_failed": fields_failed,
                "field_outcomes": field_outcomes,
            }),
        );
        if fields_failed > 0 {
            let _ = log_event(
                log_file,
                "error",
                "discovery.all_fields_failed",
                json!({
                    "object_id": item.object_id,
                    "relation_id": relation.id,
                    "fields_failed": fields_failed
                }),
            );
            return Err(anyhow::anyhow!(
                "all translatable fields failed for object {} (relation_id={}, fields_failed={})",
                item.object_id,
                relation.id,
                fields_failed
            ));
        }
        return Ok(None);
    }

    if fields_failed > 0 {
        let _ = log_event(
            log_file,
            "warning",
            "discovery.partial_translation",
            json!({
                "object_id": item.object_id,
                "relation_id": relation.id,
                "fields_translated": translated_fields.len(),
                "fields_failed": fields_failed,
                "fields_total": translate_fields.len()
            }),
        );
    }

    // Build payload
    let translated_fields_map: HashMap<String, String> = translated_fields
        .iter()
        .filter_map(|(k, v)| match v {
            Value::String(s) => Some((k.clone(), s.clone())),
            Value::Null => None,
            other => Some((k.clone(), serde_json::to_string(other).unwrap_or_default())),
        })
        .collect();

    let translated_meta_map: HashMap<String, String> = translated_meta
        .iter()
        .filter_map(|(k, v)| match v {
            Value::String(s) => Some((k.clone(), s.clone())),
            Value::Null => None,
            other => Some((k.clone(), serde_json::to_string(other).unwrap_or_default())),
        })
        .collect();
    let mut media_mappings: Vec<MediaMapping> = media_mappings_by_source.into_values().collect();
    // A text rule for an attachment translates its title/caption/alt metadata,
    // but does not itself describe the attachment binary.  Add the authenticated
    // source-copy mapping unless a media-capable rule has already supplied one.
    if item.subtype == "attachment" && media_mappings.is_empty() {
        if let Some(source_ref) = complete_data
            .get("attachment_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            media_mappings.push(MediaMapping {
                source_id: u64::try_from(item.object_id).unwrap_or(0),
                translated_ref: source_ref.to_string(),
                attachment_id: None,
                source_copy: true,
            });
        }
    }
    media_mappings.sort_by_key(|m| m.source_id);
    field_results.sort_by(|a, b| a.field.cmp(&b.field).then(a.status.cmp(&b.status)));

    let canonical_object_type = match item.object_type.as_str() {
        // Wire contract is post_type/taxonomy only; map aliases from older in-memory items.
        "post" | "post_type" => "post_type".to_string(),
        "term" | "taxonomy" => "taxonomy".to_string(),
        other => other.to_string(),
    };
    let source_revision = item
        .complete_data
        .get("__wptsall_job_snapshot")
        .and_then(|snapshot| snapshot.get("source_revision"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let source_revision = if source_revision.is_empty()
        && false
        && (item.subtype != "attachment" || item.complete_data.get("attachment_url").is_none())
    {
        // Most unit fixtures predate the claim-time snapshot contract. Keep
        // those fixtures focused on field routing while production remains
        // strict; attachment source-copy tests intentionally still exercise
        // the missing-snapshot rejection below.
        "unit-test-source-revision".to_string()
    } else {
        source_revision
    };
    if source_revision.is_empty() {
        eprintln!(
            "skipping callback: missing source_revision job snapshot (object_id={}, object_type={})",
            item.object_id, canonical_object_type
        );
        return Err(anyhow::anyhow!(
            "missing source_revision for object {} ({})",
            item.object_id,
            canonical_object_type
        ));
    }
    let object_snapshot_hash = compute_object_snapshot_hash(&item.complete_data);
    let snapshot_fp = if object_snapshot_hash.is_empty() {
        "nohash"
    } else {
        &object_snapshot_hash[..std::cmp::min(12, object_snapshot_hash.len())]
    };
    let domain_fp = short_domain_fingerprint(wp_base);
    let idempotency_key = format!(
        "discovery-{}-{}-{}-{}-{}",
        domain_fp, relation.id, canonical_object_type, item.object_id, snapshot_fp
    );

    let execution_time_ms = item_start.elapsed().as_millis() as u64;
    let attempt_id =
        build_callback_attempt_id(&worker_config.worker_id, relation.id, item.object_id);

    let payload = TranslationCallbackPayload {
        schema_version: crate::config::TASK_CALLBACK_SCHEMA_VERSION,
        attempt_id,
        object_snapshot_hash,
        source_revision,
        policy_version: item
            .complete_data
            .get("__wptsall_job_snapshot")
            .and_then(|snapshot| snapshot.get("policy_version"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        field_results,
        relation_id: u64::try_from(relation.id).unwrap_or(0),
        business_line: business_line.to_string(),
        object_type: canonical_object_type,
        subtype: item.subtype.clone(),
        object_id: u64::try_from(item.object_id).unwrap_or(0),
        translated_fields: translated_fields_map,
        translated_meta: translated_meta_map,
        media_mappings,
        media_field_sources,
        client_task_id: idempotency_key.clone(),
        outbox_id: item
            .complete_data
            .get("_wptsall_outbox_id")
            .and_then(Value::as_u64),
        worker_id: worker_config.worker_id.clone(),
        source_lang: relation.source_lang.clone(),
        target_lang: relation.target_lang.clone(),
        execution_time_ms,
    };

    let mut trace = TranslationBuildTrace {
        payload,
        idempotency_key,
        component_ids: used_component_ids.into_iter().collect(),
    };
    bind_manual_callback(&mut trace, async_scope.as_ref())?;
    Ok(Some(trace))
}

fn bind_manual_callback(
    trace: &mut TranslationBuildTrace,
    scope: Option<&crate::db::async_jobs::AsyncJobScope>,
) -> anyhow::Result<()> {
    if let Some(request) = scope
        .and_then(|scope| scope.source_snapshot.as_ref())
        .and_then(|snapshot| snapshot.get("manual_generation"))
        .and_then(Value::as_str)
    {
        crate::db::review_attempts::validate_request(request)?;
        trace.idempotency_key = format!("manual-{request}");
        trace.payload.client_task_id = trace.idempotency_key.clone();
        trace.payload.attempt_id = request.to_string();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn translate_item_fields_with_trace(
    client: &Client,
    wp_base: &str,
    item: &ContentItem,
    relation: &DiscoveredRelation,
    rules: &[DiscoveredRule],
    component_registry: Option<&ComponentRuntimeRegistry>,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
    rule_component_bindings: Option<&RuleComponentBindingsDoc>,
    worker_config: &WorkerConfig,
    log_file: &str,
) -> anyhow::Result<Option<TranslationBuildTrace>> {
    translate_item_fields_with_trace_using_proxy(
        client,
        None,
        wp_base,
        item,
        relation,
        rules,
        component_registry,
        component_id_override,
        component_prefer_ids,
        task_type_component_bindings,
        rule_component_bindings,
        worker_config,
        log_file,
        // FL-9: legacy no-task callers have no (domain, relation) task
        // context — keep the unguarded fallback behavior.
        None,
        // GAP-04: legacy callers have no db scope — async jobs stay
        // in-memory for them (pre-GAP-04 behavior).
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn translate_item_fields(
    client: &Client,
    wp_base: &str,
    item: &ContentItem,
    relation: &DiscoveredRelation,
    rules: &[DiscoveredRule],
    component_registry: Option<&ComponentRuntimeRegistry>,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
    rule_component_bindings: Option<&RuleComponentBindingsDoc>,
    worker_config: &WorkerConfig,
    log_file: &str,
) -> anyhow::Result<Option<(TranslationCallbackPayload, String)>> {
    Ok(translate_item_fields_with_trace(
        client,
        wp_base,
        item,
        relation,
        rules,
        component_registry,
        component_id_override,
        component_prefer_ids,
        task_type_component_bindings,
        rule_component_bindings,
        worker_config,
        log_file,
    )
    .await?
    .map(|trace| (trace.payload, trace.idempotency_key)))
}

// ---------------------------------------------------------------------------
// Persistence functions
// ---------------------------------------------------------------------------

/// Save raw content to disk and update item status: pending → fetched.
pub(crate) async fn persist_raw_content(
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    item_db_id: i64,
    raw_content: &Value,
    raw_path: &str,
    log_file: &str,
) -> anyhow::Result<()> {
    let lease = crate::db::unit_lock::UnitLease::item(db, item_db_id).await?;
    persist_raw_content_claimed(&lease, db, item_db_id, raw_content, raw_path, log_file).await
}

async fn persist_raw_content_claimed(
    lease: &crate::db::unit_lock::UnitLease,
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    item_db_id: i64,
    raw_content: &Value,
    raw_path: &str,
    log_file: &str,
) -> anyhow::Result<()> {
    let mut conn = db.lock().await;
    let tx = conn.savepoint()?;
    lease.assert_item(&tx, db, item_db_id)?;
    let item = crate::db::jobs::get_item_checked(&tx, item_db_id)?
        .context("raw persistence item missing")?;
    anyhow::ensure!(
        matches!(item.status.as_str(), "pending" | "fetching" | "fetched"),
        "raw persistence cannot reset a delivered or reviewed item"
    );
    save_immutable_json(std::path::Path::new(raw_path), raw_content)?;
    update_item_status(&tx, item_db_id, "fetched", None)?;
    lease.assert_item(&tx, db, item_db_id)?;
    tx.commit()?;
    drop(conn);

    let _ = log_event(
        log_file,
        "info",
        "pipeline.raw_persisted",
        json!({
            "item_id": item_db_id,
            "raw_path": raw_path
        }),
    );

    Ok(())
}

/// Save translated payload to disk and update item status: fetched → translated.
///
/// The disk file contains the full callback payload JSON plus metadata,
/// enabling crash recovery (read payload from disk → re-submit).
pub(crate) async fn persist_translated(
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    item_db_id: i64,
    payload: &TranslationCallbackPayload,
    idempotency_key: &str,
    route_secret: Option<&str>,
    translated_path: &str,
    log_file: &str,
) -> anyhow::Result<()> {
    let lease = crate::db::unit_lock::UnitLease::item(db, item_db_id).await?;
    persist_translated_claimed(
        &lease,
        db,
        item_db_id,
        payload,
        idempotency_key,
        route_secret,
        translated_path,
        log_file,
        None,
    )
    .await
}

/// Complete files precede the atomic item/path/manual-attempt projection.
/// A failed projection retains a complete orphan, never truncates prior evidence.
pub(crate) fn save_immutable_json(path: &std::path::Path, value: &Value) -> anyhow::Result<()> {
    let encoded = prepare_json_snapshot(path, value)?;
    install_json_snapshot(path, value, &encoded)
}

pub(crate) fn prepare_json_snapshot(
    path: &std::path::Path,
    value: &Value,
) -> anyhow::Result<Vec<u8>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_file(),
                "saved artifact is not a regular file"
            );
            let bytes = std::fs::read(path)?;
            let saved: Value = serde_json::from_str(&crate::bindings::decrypt_from_bytes(&bytes)?)
                .context("existing saved artifact is damaged; retained")?;
            anyhow::ensure!(
                saved == *value,
                "existing saved artifact differs; original retained"
            );
            Ok(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::bindings::encrypt_for_save(&serde_json::to_string_pretty(value)?)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn install_json_snapshot(
    path: &std::path::Path,
    value: &Value,
    encoded: &[u8],
) -> anyhow::Result<()> {
    let logical: Value = serde_json::from_str(&crate::bindings::decrypt_from_bytes(encoded)?)?;
    anyhow::ensure!(logical == *value, "prepared artifact differs; retained");
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_file() && std::fs::read(path)? == encoded,
                "saved artifact bytes differ; original retained"
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::bindings::atomic_file::install_new(path, encoded)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn save_immutable_encrypted_json(
    path: &std::path::Path,
    value: &Value,
) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_file() && std::fs::read(path)?.starts_with(b"WPTC"),
                "encrypted artifact was changed; original retained"
            );
            let saved: Value =
                serde_json::from_str(&crate::bindings::load_encrypted_or_plain(path)?)?;
            anyhow::ensure!(
                saved == *value,
                "encrypted artifact differs; original retained"
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let encoded = crate::bindings::encrypt_for_save(&serde_json::to_string(value)?)?;
            crate::bindings::atomic_file::install_new(path, &encoded)
        }
        Err(error) => Err(error.into()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_translated_claimed(
    lease: &crate::db::unit_lock::UnitLease,
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    item_db_id: i64,
    payload: &TranslationCallbackPayload,
    idempotency_key: &str,
    _route_secret: Option<&str>,
    translated_path: &str,
    log_file: &str,
    manual_scope: Option<&crate::db::async_jobs::AsyncJobScope>,
) -> anyhow::Result<()> {
    let envelope = json!({
        "idempotency_key": idempotency_key,
        "payload": payload,
    });
    let prepared = if let Some(scope) = manual_scope {
        Some(
            crate::db::review_attempts::prepare_result(
                db,
                lease,
                scope,
                item_db_id,
                &envelope,
                translated_path,
            )
            .await?,
        )
    } else {
        None
    };
    let mut conn = db.lock().await;
    let tx = conn.savepoint()?;
    lease.assert_item(&tx, db, item_db_id)?;
    let item = crate::db::jobs::get_item_checked(&tx, item_db_id)?
        .context("result persistence item missing")?;
    anyhow::ensure!(
        !matches!(item.status.as_str(), "done" | "skipped"),
        "result persistence cannot reset a delivered item"
    );
    if let Some(encoded) = prepared {
        install_json_snapshot(std::path::Path::new(translated_path), &envelope, &encoded)?;
    } else {
        save_immutable_json(std::path::Path::new(translated_path), &envelope)?;
    }
    project_saved_translation_claimed(
        &tx,
        lease,
        db,
        item_db_id,
        payload,
        translated_path,
        manual_scope,
    )?;
    tx.commit()?;
    drop(conn);

    let _ = log_event(
        log_file,
        "info",
        "pipeline.translated_persisted",
        json!({
            "item_id": item_db_id,
            "path": translated_path
        }),
    );
    Ok(())
}

pub(crate) fn project_saved_translation_claimed(
    conn: &rusqlite::Connection,
    lease: &crate::db::unit_lock::UnitLease,
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    item_db_id: i64,
    payload: &TranslationCallbackPayload,
    translated_path: &str,
    manual_scope: Option<&crate::db::async_jobs::AsyncJobScope>,
) -> anyhow::Result<()> {
    lease.assert_item(conn, db, item_db_id)?;
    let item = crate::db::jobs::get_item_checked(conn, item_db_id)?
        .context("result persistence item missing")?;
    anyhow::ensure!(
        !matches!(item.status.as_str(), "done" | "skipped"),
        "result persistence cannot reset a delivered item"
    );
    anyhow::ensure!(
        conn.execute(
            "UPDATE translation_items SET client_task_id=?1 WHERE id=?2",
            rusqlite::params![payload.client_task_id, item_db_id],
        )? == 1,
        "result identity was not committed; original state retained"
    );
    update_item_translated_path(conn, item_db_id, translated_path)?;
    update_item_status(
        conn,
        item_db_id,
        if manual_scope.is_some() {
            "pending_review"
        } else {
            "translated"
        },
        None,
    )?;
    if let Some(scope) = manual_scope {
        crate::db::review_attempts::finish(conn, db, lease, scope, item_db_id)?;
    }
    lease.assert_item(conn, db, item_db_id)?;
    Ok(())
}

/// Read translated payload from disk, submit callback to WP, update item status: translated → done.
///
/// On callback failure, the item stays at "translated" status — next run will
/// retry via `list_resumable_items()`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn sync_item_to_wp(
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    client: &Client,
    item_db_id: i64,
    translated_path: &str,
    wp_base: &str,
    token: &str,
    worker_config: &WorkerConfig,
    route_secret: Option<&str>,
    log_file: &str,
    callback_sem: &Arc<Semaphore>,
) -> anyhow::Result<()> {
    let lease = crate::db::unit_lock::UnitLease::item(db, item_db_id).await?;
    sync_item_to_wp_claimed(
        &lease,
        db,
        client,
        item_db_id,
        translated_path,
        wp_base,
        token,
        worker_config,
        route_secret,
        log_file,
        callback_sem,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn sync_item_to_wp_claimed(
    lease: &crate::db::unit_lock::UnitLease,
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    client: &Client,
    item_db_id: i64,
    translated_path: &str,
    wp_base: &str,
    token: &str,
    worker_config: &WorkerConfig,
    route_secret: Option<&str>,
    log_file: &str,
    callback_sem: &Arc<Semaphore>,
) -> anyhow::Result<()> {
    {
        let conn = db.lock().await;
        lease.assert_item(&conn, db, item_db_id)?;
    }
    // 1. Read envelope from disk
    let raw_bytes = std::fs::read(translated_path).with_context(|| {
        format!(
            "pipeline: failed to read translated file {}",
            translated_path
        )
    })?;
    let envelope: Value = serde_json::from_str(&crate::bindings::decrypt_from_bytes(&raw_bytes)?)
        .with_context(|| {
        format!(
            "pipeline: failed to parse translated file {}",
            translated_path
        )
    })?;

    let mut payload: TranslationCallbackPayload =
        serde_json::from_value(envelope.get("payload").cloned().unwrap_or(Value::Null))
            .context("pipeline: failed to deserialize payload from translated file")?;

    let idempotency_key = envelope
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Use route_secret from parameter (current session) over persisted value
    let effective_route_secret_owned = route_secret.map(str::to_string).or_else(|| {
        envelope
            .get("route_secret")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });
    let effective_route_secret = effective_route_secret_owned.as_deref();

    if !payload.media_mappings.is_empty() {
        let run_media = {
            let conn = db.lock().await;
            let dsl = crate::task_engine::workflow_dsl::load_workflow_dsl(&conn);
            crate::task_engine::workflow_interpreter::should_run_media_step(&dsl)
        };
        if run_media {
            upload_pending_media_durable(
                db,
                client,
                wp_base,
                token,
                &worker_config.worker_id,
                &worker_config.device_id,
                // Local SQLite item id is not a WP tasks.id — omit unless this
                // envelope carries a real WP lifecycle task (outbox path sets it).
                envelope
                    .get("wp_task_id")
                    .and_then(|v| v.as_i64())
                    .or_else(|| envelope.get("task_id").and_then(|v| v.as_i64()))
                    .filter(|id| *id > 0)
                    .unwrap_or(0),
                payload.relation_id,
                &mut payload,
                log_file,
                effective_route_secret,
            )
            .await?;

            // Paid receipt hashes cover these physical bytes. The encrypted
            // pending callback below is the delivery projection, not a rewrite.
            anyhow::ensure!(
                std::fs::read(translated_path)? == raw_bytes,
                "paid result changed during media delivery; original retained"
            );
        }
    }

    // 2. Save as pending callback in DB (for crash recovery before WP ack)
    let relation_id_i64 = i64::try_from(payload.relation_id).unwrap_or(0);
    let object_id_i64 = i64::try_from(payload.object_id).unwrap_or(0);
    let callback_authority = {
        let conn = db.lock().await;
        let entry = PendingCallbackEntry {
            api_base_url: wp_base.to_string(),
            idempotency_key: idempotency_key.clone(),
            payload: payload.clone(),
            route_secret: effective_route_secret_owned.clone(),
            created_at: unix_ts(),
            retry_count: 0,
            last_retry_at: 0,
            relation_id: relation_id_i64,
            object_id: object_id_i64,
            object_type: payload.object_type.clone(),
        };
        crate::storage_capacity::with_database_credit(None, || {
            add_pending_callback(&conn, &entry)
                .context("pipeline: cannot persist encrypted pending callback")?;
            crate::db::pending_callbacks::continuation_authority(
                &conn,
                wp_base,
                &idempotency_key,
                &payload,
                effective_route_secret,
            )
        })?
    };

    // 3. Submit callback (with semaphore + retry)
    let _cb_permit = callback_sem
        .acquire()
        .await
        .context("callback budget is closed; retained")?;
    let cb_result = {
        let client_c = client.clone();
        let wp_base_c = wp_base.to_string();
        let token_c = token.to_string();
        let worker_id_c = worker_config.worker_id.clone();
        let device_id_c = worker_config.device_id.clone();
        let idempotency_key_c = idempotency_key.clone();
        let payload_c = payload.clone();
        retry_with_backoff(
            "pipeline.sync_callback",
            relation_id_i64,
            log_file,
            worker_config,
            |_attempt| {
                let cl = client_c.clone();
                let wb = wp_base_c.clone();
                let tk = token_c.clone();
                let wi = worker_id_c.clone();
                let di = device_id_c.clone();
                let ik = idempotency_key_c.clone();
                let pl = payload_c.clone();
                async move { send_translation_callback(&cl, &wb, &tk, &wi, &di, &ik, &pl).await }
            },
        )
        .await
    };

    match cb_result {
        Ok(ack) => {
            // 4. Commit the ack, item/job and scan dedup marker. Retained
            // files are not permission to replay or translate a done item.
            {
                let mut conn = db.lock().await;
                lease.assert_item(&conn, db, item_db_id)?;
                anyhow::ensure!(
                    crate::db::pending_callbacks::continuation_authority(
                        &conn,
                        wp_base,
                        &idempotency_key,
                        &payload,
                        effective_route_secret,
                    )? == callback_authority,
                    "pending callback changed after original request; receipt retained"
                );
                let credit = match conn
                    .path()
                    .filter(|path| !path.is_empty() && callback_authority.1)
                {
                    Some(path) => crate::storage_capacity::database_recovery_credit(
                        std::path::Path::new(path),
                        false,
                    )?,
                    None => None,
                };
                crate::storage_capacity::with_database_credit(credit, || -> anyhow::Result<()> {
                    let tx = conn.savepoint()?;
                    lease.assert_item(&tx, db, item_db_id)?;
                    anyhow::ensure!(
                        crate::db::pending_callbacks::continuation_authority(
                            &tx,
                            wp_base,
                            &idempotency_key,
                            &payload,
                            effective_route_secret,
                        )? == callback_authority
                            && std::fs::read(translated_path)? == raw_bytes,
                        "original callback authority or result changed; receipt retained"
                    );
                    crate::db::jobs::complete_saved_callback(
                        &tx,
                        item_db_id,
                        wp_base,
                        &idempotency_key,
                        &serde_json::to_string(&ack)?,
                    )?;
                    tx.commit()?;
                    Ok(())
                })?;
            }

            // Completed raw/result artifacts default to manual retention.

            // FO-1 (Wave-2): the review-approve writeback used to record ONLY
            // pipeline.sync_done, while the auto-translate path additionally
            // emits discovery.callback_sent — the audit chain was inconsistent
            // between the two writeback paths (sim-04 wire evidence: the
            // callback physically arrived with no callback audit event).
            // Emit the unified callback audit here too, with a `via`
            // discriminator so the chain stays attributable.
            let _ = log_event(
                log_file,
                "info",
                "discovery.callback_sent",
                json!({
                    "relation_id": payload.relation_id,
                    "object_id": payload.object_id,
                    "via": "review_writeback",
                }),
            );
            let _ = log_event(
                log_file,
                "info",
                "pipeline.sync_done",
                json!({
                    "item_id": item_db_id,
                    "relation_id": payload.relation_id,
                    "object_id": payload.object_id
                }),
            );
            Ok(())
        }
        Err(err) => {
            // Callback failed — item stays at "translated", will be retried
            {
                let mut conn = db.lock().await;
                let tx = conn.savepoint()?;
                lease.assert_item(&tx, db, item_db_id)?;
                crate::db::jobs::increment_item_retry(&tx, item_db_id)?;
                let current = crate::db::jobs::get_item_checked(&tx, item_db_id)?;
                if let Some(item) = current {
                    let next_status = super::delivery_policy::status_after_callback_failure(
                        item.retry_count,
                        item.max_retries,
                    );
                    let detail = if next_status == "failed" {
                        format!(
                            "callback retry budget exhausted: {}",
                            snippet(&format!("{:#}", err))
                        )
                    } else {
                        snippet(&format!("{:#}", err))
                    };
                    update_item_status(&tx, item_db_id, next_status, Some(&detail))?;
                }
                tx.commit()?;
            }
            let _ = log_event(
                log_file,
                "warning",
                "pipeline.sync_callback_failed",
                json!({
                    "item_id": item_db_id,
                    "relation_id": payload.relation_id,
                    "object_id": payload.object_id,
                    "error": snippet(&format!("{:#}", err))
                }),
            );
            Err(err)
        }
    }
}

/// Payload-shape probe mirroring the WP plugin's callback routing
/// (class-client-data-rest-controller.php — the *_i18n family "sometimes
/// classifies CPT/config objects as *_i18n but still submits content-shaped
/// payloads (translated_fields/meta) without entries" and accepts them via
/// the content write-back path). The i18n lane must route the same way:
/// a business_line of config_i18n with a post/option object translates FIELD
/// maps, and its translated file never carries po-style `entries` — routing
/// it into the entries channel deserialized it into I18nCallbackPayload and
/// hard-400ed the sync with "missing field `entries`". Envelope handling
/// matches sync_i18n_item_to_wp (payload nested or bare); unreadable/
/// unparseable files keep the legacy i18n routing so genuine packs surface
/// their own errors unchanged.
pub(crate) fn translated_payload_has_i18n_entries(translated_path: &str) -> bool {
    let Ok(raw) = crate::bindings::load_encrypted_or_plain(std::path::Path::new(translated_path))
    else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return true;
    };
    let payload = value.get("payload").unwrap_or(&value);
    i18n_payload_value_has_entries(payload)
}

/// Value-level core of [`translated_payload_has_i18n_entries`]: a non-empty
/// `entries` array means po-style i18n payload; anything else (absent,
/// empty, non-array) is content-shaped.
pub(crate) fn i18n_payload_value_has_entries(payload: &Value) -> bool {
    payload
        .get("entries")
        .and_then(|entries| entries.as_array())
        .map_or(false, |entries| !entries.is_empty())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn sync_i18n_item_to_wp(
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    client: &Client,
    item_db_id: i64,
    translated_path: &str,
    wp_base: &str,
    token: &str,
    worker_config: &crate::types::WorkerConfig,
    log_file: &str,
    callback_sem: &Arc<Semaphore>,
) -> anyhow::Result<usize> {
    let lease = crate::db::unit_lock::UnitLease::item(db, item_db_id).await?;
    sync_i18n_item_to_wp_claimed(
        &lease,
        db,
        client,
        item_db_id,
        translated_path,
        wp_base,
        token,
        worker_config,
        log_file,
        callback_sem,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn sync_i18n_item_to_wp_claimed(
    lease: &crate::db::unit_lock::UnitLease,
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    client: &Client,
    item_db_id: i64,
    translated_path: &str,
    wp_base: &str,
    token: &str,
    worker_config: &crate::types::WorkerConfig,
    log_file: &str,
    callback_sem: &Arc<Semaphore>,
) -> anyhow::Result<usize> {
    {
        let conn = db.lock().await;
        lease.assert_item(&conn, db, item_db_id)?;
    }
    let raw_bytes = std::fs::read(translated_path)
        .with_context(|| format!("read translated file failed: {}", translated_path))?;
    let envelope: serde_json::Value =
        serde_json::from_str(&crate::bindings::decrypt_from_bytes(&raw_bytes)?)
            .with_context(|| format!("parse translated file failed: {}", translated_path))?;

    // Shape routing (mirrors the WP plugin's callback controller and the
    // review path's probe): a file that reached the i18n channel but carries
    // a content-shaped payload (no po-style entries — e.g. a config_i18n
    // relation translating a wp_global_styles CPT via json_structured
    // fields) must not hard-fail with "missing field `entries`". Delegate
    // to the content callback lane, which deserializes
    // TranslationCallbackPayload, persists the pending callback, and
    // submits through send_translation_callback. Files that DO carry
    // entries but still fail to deserialize surface their genuine errors
    // unchanged.
    let payload_value = envelope
        .get("payload")
        .cloned()
        .unwrap_or_else(|| envelope.clone());
    if !i18n_payload_value_has_entries(&payload_value) {
        let _ = log_event(
            log_file,
            "info",
            "pipeline.sync_i18n_shape_fallback",
            json!({
                "item_id": item_db_id,
                "translated_path": translated_path,
            }),
        );
        let route_secret = envelope
            .get("route_secret")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        return sync_item_to_wp_claimed(
            lease,
            db,
            client,
            item_db_id,
            translated_path,
            wp_base,
            token,
            worker_config,
            route_secret.as_deref(),
            log_file,
            callback_sem,
        )
        .await
        .map(|_| 0);
    }

    let payload: crate::types::I18nCallbackPayload = serde_json::from_value(payload_value)
        .context("deserialize i18n payload from translated file failed")?;
    let idempotency_key = envelope
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| payload.client_task_id.clone());
    let relation_id_i64 = i64::try_from(payload.relation_id).unwrap_or(0);
    let entry_count = payload.entries.len();
    let receipt_key = format!("language-pack-callback-v1:{item_db_id}");
    let item = {
        let mut conn = db.lock().await;
        let tx = conn.savepoint()?;
        lease.assert_item(&tx, db, item_db_id)?;
        let item = crate::db::jobs::get_item_checked(&tx, item_db_id)?
            .context("saved language-pack item disappeared; retained")?;
        anyhow::ensure!(
            item.translated_path == translated_path
                && item.client_task_id == payload.client_task_id
                && idempotency_key == payload.client_task_id
                && item.relation_id == relation_id_i64
                && item.business_line == payload.business_line
                && item
                    .effective_source_lang
                    .as_deref()
                    .unwrap_or(&item.source_lang)
                    == payload.source_lang
                && item
                    .effective_target_lang
                    .as_deref()
                    .unwrap_or(&item.target_lang)
                    == payload.target_lang,
            "language-pack callback scope differs from saved item; retained"
        );
        let saved = crate::db::system::get_system_config_checked(&tx, &receipt_key)?;
        if item.status == "done" {
            let raw = saved.context("completed language-pack receipt is unprovable; retained")?;
            anyhow::ensure!(
                raw.starts_with("V1BUQw"),
                "language-pack receipt is not encrypted; retained"
            );
            let receipt: Value =
                serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)?;
            anyhow::ensure!(
                receipt["format"] == "language-pack-callback-v1"
                    && receipt["item_id"] == item_db_id
                    && receipt["site"] == wp_base
                    && receipt["idempotency_key"] == idempotency_key
                    && receipt["payload"] == serde_json::to_value(&payload)?,
                "completed language-pack receipt scope differs; retained"
            );
            crate::task_engine::submitter::validate_i18n_callback_ack(
                receipt["ack"].clone(),
                entry_count,
            )?;
            language_pack::assert_no_delivery(&tx, item_db_id)?;
            return Ok(0);
        }
        anyhow::ensure!(
            saved.is_none(),
            "language-pack applied receipt/item conflict; retained"
        );
        tx.commit()?;
        item
    };
    let _cb_permit = callback_sem
        .acquire()
        .await
        .context("callback budget is closed; retained")?;
    let (delivery_intent, delivery_authority) = {
        let mut conn = db.lock().await;
        crate::storage_capacity::with_database_credit(
            None,
            || -> anyhow::Result<(String, String)> {
                let tx = conn.savepoint()?;
                lease.assert_item(&tx, db, item_db_id)?;
                let current = crate::db::jobs::get_item_checked(&tx, item_db_id)?
                    .context("saved language-pack item disappeared; retained")?;
                anyhow::ensure!(
                    serde_json::to_value(&current)? == serde_json::to_value(&item)?,
                    "language-pack item changed before delivery; retained"
                );
                let intent = language_pack::prepare_delivery(
                    &tx,
                    db,
                    lease,
                    &item,
                    wp_base,
                    &payload,
                    &idempotency_key,
                )?;
                let authority = language_pack::delivery_authority(
                    &tx,
                    &item,
                    wp_base,
                    &payload,
                    &idempotency_key,
                    &intent,
                )?;
                tx.commit()?;
                Ok((intent, authority))
            },
        )?
    };
    let cb_result = retry_with_backoff(
        "pipeline.sync_i18n_callback",
        relation_id_i64,
        log_file,
        worker_config,
        |_attempt| {
            let client = client.clone();
            let wp_base = wp_base.to_string();
            let token = token.to_string();
            let worker_id = worker_config.worker_id.clone();
            let device_id = worker_config.device_id.clone();
            let idempotency_key = idempotency_key.clone();
            let payload = payload.clone();
            async move {
                send_i18n_translation_callback(
                    &client,
                    &wp_base,
                    &token,
                    &worker_id,
                    &device_id,
                    &idempotency_key,
                    &payload,
                )
                .await
            }
        },
    )
    .await;

    match cb_result {
        Ok(ack) => {
            {
                let mut conn = db.lock().await;
                lease.assert_item(&conn, db, item_db_id)?;
                let current = crate::db::jobs::get_item_checked(&conn, item_db_id)?
                    .context("saved language-pack item disappeared; retained")?;
                anyhow::ensure!(
                    serde_json::to_value(&current)? == serde_json::to_value(&item)?
                        && std::fs::read(translated_path)? == raw_bytes,
                    "original language-pack item or result changed; retained"
                );
                anyhow::ensure!(
                    language_pack::delivery_authority(
                        &conn,
                        &item,
                        wp_base,
                        &payload,
                        &idempotency_key,
                        &delivery_intent,
                    )? == delivery_authority,
                    "original language-pack delivery changed; retained"
                );
                let credit = match conn
                    .path()
                    .filter(|path| !path.is_empty() && delivery_intent.starts_with("V1BUQw"))
                {
                    Some(path) => crate::storage_capacity::database_recovery_credit(
                        std::path::Path::new(path),
                        false,
                    )?,
                    None => None,
                };
                crate::storage_capacity::with_database_credit(credit, || -> anyhow::Result<()> {
                    let tx = conn.savepoint()?;
                    lease.assert_item(&tx, db, item_db_id)?;
                    let current = crate::db::jobs::get_item_checked(&tx, item_db_id)?
                        .context("saved language-pack item disappeared; retained")?;
                    anyhow::ensure!(
                        serde_json::to_value(&current)? == serde_json::to_value(&item)?
                            && std::fs::read(translated_path)? == raw_bytes,
                        "original language-pack item or result changed; retained"
                    );
                    anyhow::ensure!(
                        language_pack::delivery_authority(
                            &tx,
                            &item,
                            wp_base,
                            &payload,
                            &idempotency_key,
                            &delivery_intent,
                        )? == delivery_authority
                            && crate::db::system::get_system_config_checked(&tx, &receipt_key)?
                                .is_none(),
                        "language-pack delivery or callback receipt changed; retained"
                    );
                    crate::db::jobs::complete_saved_callback(
                        &tx,
                        item_db_id,
                        wp_base,
                        &idempotency_key,
                        &serde_json::to_string(&ack)?,
                    )?;
                    crate::db::system::set_encrypted_config(
                        &tx,
                        &receipt_key,
                        &json!({
                            "format":"language-pack-callback-v1","item_id":item_db_id,
                            "site":wp_base,"idempotency_key":idempotency_key,
                            "payload":payload,"ack":ack
                        })
                        .to_string(),
                    )?;
                    language_pack::finish_delivery(&tx, item_db_id, &delivery_intent)?;
                    tx.commit()?;
                    Ok(())
                })?;
            }
            // Callback acknowledgement does not authorize artifact deletion.
            let _ = log_event(
                log_file,
                "info",
                "pipeline.sync_i18n_done",
                json!({
                    "item_id": item_db_id,
                    "relation_id": payload.relation_id,
                    "entries": entry_count
                }),
            );
            Ok(entry_count)
        }
        Err(err) => {
            {
                let mut conn = db.lock().await;
                let tx = conn.savepoint()?;
                lease.assert_item(&tx, db, item_db_id)?;
                crate::db::jobs::increment_item_retry(&tx, item_db_id)?;
                let current = crate::db::jobs::get_item_checked(&tx, item_db_id)?;
                if let Some(item) = current {
                    if item.max_retries > 0 && item.retry_count >= item.max_retries {
                        update_item_status(
                            &tx,
                            item_db_id,
                            "failed",
                            Some(&format!(
                                "i18n callback retry budget exhausted: {}",
                                snippet(&format!("{:#}", err))
                            )),
                        )?;
                    } else {
                        update_item_status(
                            &tx,
                            item_db_id,
                            "translated",
                            Some(&snippet(&format!("{:#}", err))),
                        )?;
                    }
                }
                tx.commit()?;
            }
            let _ = log_event(
                log_file,
                "warning",
                "pipeline.sync_i18n_failed",
                json!({
                    "item_id": item_db_id,
                    "relation_id": payload.relation_id,
                    "entries": entry_count,
                    "error": format!("{:#}", err)
                }),
            );
            Err(err)
        }
    }
}

// ---------------------------------------------------------------------------
// fetch_item_content (kept from original)
// ---------------------------------------------------------------------------

/// Write `raw_content` to `item.raw_path` and update the item's status in DB.
///
/// Status transitions: `pending` → `fetching` → `fetched` (or `failed`).
pub(crate) async fn fetch_item_content(
    db: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    _client: &Client,
    log_file: &str,
    item: &crate::db::jobs::TranslationItem,
    raw_content: &Value,
) -> anyhow::Result<()> {
    let lease = crate::db::unit_lock::UnitLease::item(&db, item.id).await?;
    let result =
        persist_raw_content_claimed(&lease, &db, item.id, raw_content, &item.raw_path, log_file)
            .await;
    if let Err(error) = &result {
        if error.downcast_ref::<std::io::Error>().is_some() {
            lease
                .mutate_item(&db, item.id, |conn| {
                    update_item_status(conn, item.id, "failed", Some(&format!("{error:#}")))
                })
                .await
                .context("failed to save fetch failure; original evidence retained")?;
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod field_plan_patch {
    use super::*;

    #[test]
    fn copy_writes_source_and_skip_writes_nothing() {
        let item: ContentItem = serde_json::from_value(json!({
            "object_type": "post",
            "subtype": "post",
            "object_id": 9,
            "complete_data": {
                "post_content": "Body",
                "post_excerpt": "Keep the existing target"
            }
        }))
        .expect("item");
        let complete = item.complete_data.as_object().expect("object").clone();
        let mut fields = serde_json::Map::new();
        let mut meta = serde_json::Map::new();
        let mut results = Vec::new();
        let mut failed = 0u32;
        apply_copy_and_skip_fields(
            None,
            &item,
            &complete,
            &["post_content".to_string()],
            &["post_excerpt".to_string()],
            &mut failed,
            &mut fields,
            &mut meta,
            &mut results,
        );
        assert_eq!(failed, 0);
        assert_eq!(
            fields.get("post_content").and_then(Value::as_str),
            Some("Body")
        );
        assert!(fields.get("post_excerpt").is_none());
        assert!(meta.is_empty());
        assert!(results.iter().any(|row| {
            row.field == "post_content"
                && row.detail == "copied_without_provider"
                && row.provider_component.is_empty()
        }));
        assert!(results
            .iter()
            .any(|row| row.field == "post_excerpt" && row.detail == "user_excluded"));
    }
}
