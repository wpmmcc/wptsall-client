use reqwest::Client;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::types::*;

use super::errors::{
    maybe_write_upstream_api_error, review_failed_item_payload, write_error_response,
    write_error_response_with_status, write_not_found_response,
};
use super::http::{parse_query_string, write_http_response};
use super::{
    build_local_component_runtime_for_task, component_exists_in_local_doc,
    load_local_components_runtime_doc, resolve_effective_route_secret_for_domain,
    validate_local_component_runtime_ready_for_task, validate_task_editable_overrides,
};
use crate::component_rt::loader::load_component_runtimes;

// Translation Jobs API
// ---------------------------------------------------------------------------

pub(super) async fn handle_jobs_list(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    // Parse query params: ?domain=...&limit=50&offset=0
    let params = parse_query_string(query);
    let domain: Option<String> = params.get("domain").filter(|v| !v.is_empty()).cloned();
    let limit: i64 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let offset: i64 = params
        .get("offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let (jobs, progresses) = {
        let conn = db_arc.lock().await;
        let jobs = crate::db::jobs::list_jobs(&conn, domain.as_deref(), limit, offset);
        let progresses: Vec<_> = jobs
            .iter()
            .map(|j| crate::db::jobs::get_job_progress(&conn, j.id))
            .collect();
        (jobs, progresses)
    };

    // Merge progress into each job JSON
    let items: Vec<serde_json::Value> = jobs
        .iter()
        .zip(progresses.iter())
        .map(|(j, p)| {
            let mut v = serde_json::to_value(j).unwrap_or(json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "progress".to_string(),
                    serde_json::to_value(p).unwrap_or(json!({})),
                );
            }
            v
        })
        .collect();

    let payload = json!({ "success": true, "data": { "items": items } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_job_detail(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id_str: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "job id must be an integer").await;
        }
    };
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let job = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_job(&conn, id)
    };

    match job {
        Some(j) => {
            let payload = json!({ "success": true, "data": j });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        None => write_not_found_response(socket, "NOT_FOUND", "job not found").await,
    }
}

pub(super) async fn handle_job_items(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id_str: &str,
    query: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "job id must be an integer").await;
        }
    };
    let params = parse_query_string(query);
    let status_filter: Option<String> = params.get("status").filter(|v| !v.is_empty()).cloned();
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let items = {
        let conn = db_arc.lock().await;
        crate::db::jobs::list_items_by_job(&conn, id, status_filter.as_deref())
    };

    let payload = json!({ "success": true, "data": { "items": items } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_item_content(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id_str: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "item id must be an integer").await;
        }
    };
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item(&conn, id)
    };

    let item = match item {
        Some(i) => i,
        None => return write_not_found_response(socket, "NOT_FOUND", "item not found").await,
    };

    if item.status != "pending_review" {
        return write_error_response(
            socket,
            "INVALID_STATUS",
            &format!(
                "item status is '{}', expected 'pending_review'",
                item.status
            ),
        )
        .await;
    }

    // Read raw file (required)
    let raw_content = if item.raw_path.is_empty() {
        serde_json::Value::Null
    } else {
        match std::fs::read_to_string(&item.raw_path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)),
            Err(_) => serde_json::Value::Null,
        }
    };

    // Read translated file (optional — may not exist yet)
    let translated_content = if item.translated_path.is_empty() {
        serde_json::Value::Null
    } else {
        match std::fs::read_to_string(&item.translated_path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)),
            Err(_) => serde_json::Value::Null,
        }
    };

    let payload = json!({
        "success": true,
        "data": {
            "item": item,
            "raw": raw_content,
            "translated": translated_content
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_item_translated_save(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    id_str: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "item id must be an integer").await;
        }
    };

    // Parse body: { "content": <json value> }
    let body_json: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_BODY", "body must be valid JSON").await;
        }
    };

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item(&conn, id)
    };

    let item = match item {
        Some(i) => i,
        None => return write_not_found_response(socket, "NOT_FOUND", "item not found").await,
    };

    if item.status != "pending_review" {
        return write_error_response(
            socket,
            "INVALID_STATUS",
            &format!(
                "item status is '{}', expected 'pending_review'",
                item.status
            ),
        )
        .await;
    }

    if item.translated_path.is_empty() {
        return write_error_response(socket, "NO_TRANSLATED_PATH", "item has no translated_path")
            .await;
    }

    // Create parent directories if needed
    if let Some(parent) = std::path::Path::new(&item.translated_path).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return write_error_response(socket, "IO_ERROR", &format!("create dir failed: {}", e))
                .await;
        }
    }

    // Preserve the translated envelope shape; only replace editable payload content.
    let content_to_write = body_json.get("content").unwrap_or(&body_json);
    let existing_file = std::fs::read_to_string(&item.translated_path).unwrap_or_default();
    let mut envelope: serde_json::Value =
        serde_json::from_str(&existing_file).unwrap_or_else(|_| json!({}));
    if envelope
        .get("payload")
        .and_then(|v| v.as_object())
        .is_some()
    {
        if let Some(payload_obj) = envelope.get_mut("payload").and_then(|v| v.as_object_mut()) {
            let content_obj = content_to_write.as_object().cloned().unwrap_or_default();
            if let Some(entries) = content_obj.get("entries").and_then(|v| v.as_array()) {
                payload_obj.insert(
                    "entries".to_string(),
                    serde_json::Value::Array(entries.clone()),
                );
            }
            let mut translated_fields = serde_json::Map::new();
            let mut translated_meta = serde_json::Map::new();
            for (key, value) in content_obj {
                if key.starts_with('_') {
                    translated_meta.insert(key, value);
                } else {
                    translated_fields.insert(key, value);
                }
            }
            payload_obj.insert(
                "translated_fields".to_string(),
                serde_json::Value::Object(translated_fields),
            );
            payload_obj.insert(
                "translated_meta".to_string(),
                serde_json::Value::Object(translated_meta),
            );
        }
    } else {
        envelope = content_to_write.clone();
    }

    // Write pretty-printed JSON to translated file
    let json_str = match serde_json::to_string_pretty(&envelope) {
        Ok(s) => s,
        Err(e) => {
            return write_error_response(
                socket,
                "SERIALIZE_ERROR",
                &format!("serialize failed: {}", e),
            )
            .await;
        }
    };

    if let Err(e) = std::fs::write(&item.translated_path, json_str) {
        return write_error_response(socket, "IO_ERROR", &format!("write failed: {}", e)).await;
    }

    let payload = json!({ "success": true });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_item_override_save(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    id_str: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "item id must be an integer").await;
        }
    };

    let body_json: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_BODY", "body must be valid JSON").await;
        }
    };

    let selected_component_id = body_json
        .get("component_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let effective_source_lang = body_json
        .get("source_lang")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let effective_target_lang = body_json
        .get("target_lang")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let editable_overrides = body_json.get("editable_overrides").cloned();
    let component_id_provided = body_json.get("component_id").is_some();
    let source_lang_provided = body_json.get("source_lang").is_some();
    let target_lang_provided = body_json.get("target_lang").is_some();
    let editable_overrides_provided = body_json.get("editable_overrides").is_some();

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item(&conn, id)
    };
    let item = match item {
        Some(i) => i,
        None => return write_not_found_response(socket, "NOT_FOUND", "item not found").await,
    };

    if item.status != "pending_review" {
        return write_error_response(
            socket,
            "INVALID_STATUS",
            &format!(
                "item status is '{}', expected 'pending_review'",
                item.status
            ),
        )
        .await;
    }

    let effective_selected_component_id = if component_id_provided {
        selected_component_id.clone()
    } else {
        item.selected_component_id.clone()
    };
    let effective_source_lang = if source_lang_provided {
        effective_source_lang
    } else {
        item.effective_source_lang.clone()
    };
    let effective_target_lang = if target_lang_provided {
        effective_target_lang
    } else {
        item.effective_target_lang.clone()
    };
    let effective_editable_overrides = if editable_overrides_provided {
        editable_overrides
    } else {
        item.editable_overrides.clone()
    };
    if (component_id_provided || editable_overrides_provided)
        && effective_selected_component_id.is_some()
    {
        if let Err(err) = validate_local_component_runtime_ready_for_task(
            state,
            effective_selected_component_id
                .as_deref()
                .unwrap_or_default(),
            effective_editable_overrides.as_ref(),
        )
        .await
        {
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "INVALID_COMPONENT_ID",
                &format!("{:#}", err),
            )
            .await;
        }
    } else if let Some(ref component_id) = selected_component_id {
        let local_doc = load_local_components_runtime_doc();
        if !component_exists_in_local_doc(&local_doc, component_id) {
            return write_error_response(
                socket,
                "INVALID_COMPONENT_ID",
                "selected component not found in local components",
            )
            .await;
        }
    }
    if let Err(err) = validate_task_editable_overrides(
        &item,
        effective_selected_component_id.as_deref(),
        effective_editable_overrides.as_ref(),
    ) {
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "INVALID_TASK_EDITABLE_OVERRIDES",
            &format!("{:#}", err),
        )
        .await;
    }

    {
        let conn = db_arc.lock().await;
        crate::db::jobs::update_item_task_override(
            &conn,
            id,
            effective_selected_component_id.as_deref(),
            effective_source_lang.as_deref(),
            effective_target_lang.as_deref(),
            effective_editable_overrides.as_ref(),
        )?;
    }

    let payload = json!({
        "success": true,
        "data": {
            "item_id": id,
            "component_id": effective_selected_component_id,
            "source_lang": effective_source_lang,
            "target_lang": effective_target_lang,
            "editable_overrides": effective_editable_overrides
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_item_retranslate(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id_str: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "item id must be an integer").await;
        }
    };

    let (db_arc, domain_token_bindings, client, device_id, rule_bindings, task_type_bindings) = {
        let guard = state.lock().await;
        (
            std::sync::Arc::clone(&guard.db),
            guard.domain_token_bindings.clone(),
            guard.http_client.clone(),
            guard.device_id.clone(),
            guard.rule_component_bindings.clone(),
            guard.task_type_component_bindings.clone(),
        )
    };

    let item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item(&conn, id)
    };
    let item = match item {
        Some(i) => i,
        None => return write_not_found_response(socket, "NOT_FOUND", "item not found").await,
    };

    if is_language_pack_item(&item) {
        return write_error_response(
            socket,
            "UNSUPPORTED_ITEM_TYPE",
            "language_pack retranslate is not supported in this MVP path",
        )
        .await;
    }

    if item.raw_path.is_empty() || !std::path::Path::new(&item.raw_path).exists() {
        return write_error_response(
            socket,
            "MISSING_RAW_FILE",
            "raw file does not exist; cannot retranslate",
        )
        .await;
    }

    let complete_data: serde_json::Value = match std::fs::read_to_string(&item.raw_path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
        Err(err) => {
            return write_error_response(
                socket,
                "RAW_READ_FAILED",
                &format!("failed to read raw file: {}", err),
            )
            .await;
        }
    };

    let selected_component_id = item
        .selected_component_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(item.component_id.as_str())
        .to_string();
    let registry = match build_runtime_registry_for_retranslate(
        state,
        &selected_component_id,
        item.editable_overrides.as_ref(),
        &crate::config::env_or("WPTSALL_LOG_FILE", crate::config::DEFAULT_LOG_FILE),
    )
    .await
    {
        Ok(registry) => registry,
        Err(err) => {
            return write_error_response(
                socket,
                "COMPONENT_RUNTIME_BUILD_FAILED",
                &format!("{:#}", err),
            )
            .await;
        }
    };

    let wp_client_token_fallback = crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "");
    let wp_client_token = match crate::bindings::resolve_wp_client_token_for_domain(
        &item.domain,
        &domain_token_bindings,
        &wp_client_token_fallback,
    ) {
        Some(t) => t,
        None => {
            return write_error_response(
                socket,
                "MISSING_TOKEN",
                "no WP client token configured for this domain",
            )
            .await;
        }
    };
    let route_secret = match resolve_effective_route_secret_for_domain(
        state,
        &domain_token_bindings,
        &item.domain,
    )
    .await
    {
        Ok(Some(route_secret)) => route_secret,
        Ok(None) => {
            return write_error_response(
                socket,
                "MISSING_ROUTE_SECRET",
                "route_secret is missing for this domain. Configure it in Sites, or make sure server domains are refreshed and include route_secret.",
            )
            .await;
        }
        Err(err) => {
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            return write_error_response(
                socket,
                "ROUTE_SECRET_RESOLUTION_FAILED",
                &format!("{:#}", err),
            )
            .await;
        }
    };
    let domain_base = crate::bindings::normalize_domain_base(&item.domain);
    let wp_base = match crate::bindings::build_wp_base_url(&domain_base, &route_secret) {
        Some(wp_base) => wp_base,
        None => {
            return write_error_response(
                socket,
                "MISSING_ROUTE_SECRET",
                "route_secret is missing for this domain. Configure it in Sites, or make sure server domains are refreshed and include route_secret.",
            )
            .await;
        }
    };

    let worker_config = crate::worker::build_worker_config(&device_id);
    let rules_url = format!("{}/rules?relation_id={}", wp_base, item.relation_id);
    let relations_url = format!("{}/site-relations", wp_base);
    let worker_id = worker_config.worker_id.clone();

    let relations = match crate::auth::wp_get_json_with_transport_and_secret::<RelationsResponse>(
        &client,
        &relations_url,
        &wp_client_token,
        &worker_id,
        Some(route_secret.as_str()),
    )
    .await
    {
        Ok(relations) => relations,
        Err(err) => {
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            return write_error_response(socket, "FETCH_RELATIONS_FAILED", &format!("{:#}", err))
                .await;
        }
    };
    let mut relation = match relations
        .relations
        .into_iter()
        .find(|r| r.id == item.relation_id)
    {
        Some(r) => r,
        None => {
            return write_error_response(
                socket,
                "RELATION_NOT_FOUND",
                "relation not found in WP discovery",
            )
            .await;
        }
    };
    if let Some(ref source_lang) = item.effective_source_lang {
        relation.source_lang = source_lang.clone();
    }
    if let Some(ref target_lang) = item.effective_target_lang {
        relation.target_lang = target_lang.clone();
    }

    let rules = match crate::auth::wp_get_json_with_transport_and_secret::<RulesResponse>(
        &client,
        &rules_url,
        &wp_client_token,
        &worker_id,
        Some(route_secret.as_str()),
    )
    .await
    {
        Ok(rules) => rules,
        Err(err) => {
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            return write_error_response(socket, "FETCH_RULES_FAILED", &format!("{:#}", err)).await;
        }
    };

    let content_item = ContentItem {
        object_type: item.object_type.clone(),
        subtype: item.wp_object_subtype.clone(),
        object_id: item.wp_object_id,
        needs_resync: false,
        mapping_id: None,
        complete_data,
    };

    let result = crate::task_engine::pipeline::translate_item_fields(
        &client,
        &wp_base,
        &content_item,
        &relation,
        &rules.rules,
        Some(&registry),
        &selected_component_id,
        &[],
        Some(&task_type_bindings),
        Some(&rule_bindings),
        &worker_config,
        &crate::config::env_or("WPTSALL_LOG_FILE", crate::config::DEFAULT_LOG_FILE),
    )
    .await;

    let (payload, idempotency_key) = match result {
        Ok(Some(v)) => v,
        Ok(None) => {
            return write_error_response(
                socket,
                "NO_TRANSLATABLE_FIELDS",
                "item has no translatable fields after retranslate",
            )
            .await;
        }
        Err(err) => {
            return write_error_response(socket, "RETRANSLATE_FAILED", &format!("{:#}", err)).await;
        }
    };

    let translated_path = if item.translated_path.trim().is_empty() {
        crate::task_engine::pipeline::build_translated_path(
            &item.raw_path,
            &crate::config::env_or("WPTSALL_DATA_DIR", crate::config::DEFAULT_DATA_DIR),
            &crate::task_engine::pipeline::sanitize_domain_key(&item.domain),
        )
    } else {
        item.translated_path.clone()
    };

    if let Err(err) = crate::task_engine::pipeline::persist_translated(
        &db_arc,
        item.id,
        &payload,
        &idempotency_key,
        Some(route_secret.as_str()),
        &translated_path,
        &crate::config::env_or("WPTSALL_LOG_FILE", crate::config::DEFAULT_LOG_FILE),
    )
    .await
    {
        return write_error_response(socket, "PERSIST_TRANSLATED_FAILED", &format!("{:#}", err))
            .await;
    }

    let payload = json!({
        "success": true,
        "data": {
            "item_id": item.id,
            "status": "translated",
            "component_id": selected_component_id,
            "source_lang": relation.source_lang,
            "target_lang": relation.target_lang,
            "translated_path": translated_path
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(crate) async fn build_runtime_registry_for_retranslate(
    state: &Arc<Mutex<WebUiState>>,
    selected_component_id: &str,
    task_editable_overrides: Option<&serde_json::Value>,
    log_file: &str,
) -> anyhow::Result<ComponentRuntimeRegistry> {
    let selected_component_id = selected_component_id.trim();
    if selected_component_id.is_empty() {
        anyhow::bail!("component_id is required for retranslate");
    }

    let local_doc = load_local_components_runtime_doc();
    let selected_is_local = component_exists_in_local_doc(&local_doc, selected_component_id);
    let local_runtime = if selected_is_local {
        Some(
            build_local_component_runtime_for_task(
                state,
                selected_component_id,
                task_editable_overrides,
            )
            .await?,
        )
    } else {
        None
    };

    let (server_base, session_token, client, component_bindings_path, mut component_bindings, db) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
            guard.component_bindings_path.clone(),
            guard.component_bindings.clone(),
            std::sync::Arc::clone(&guard.db),
        )
    };

    let session_token = session_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(session_token) = session_token {
        let target_component_ids =
            std::collections::BTreeSet::from([selected_component_id.to_string()]);
        let signing_key_from_db = {
            let conn = db.lock().await;
            crate::db::system::get_signing_key(&conn)
                .filter(|pem| pem.trim().starts_with("-----BEGIN PUBLIC KEY-----"))
        };
        match load_component_runtimes(
            &client,
            &server_base,
            session_token,
            log_file,
            &mut component_bindings,
            &component_bindings_path,
            Some(&target_component_ids),
            signing_key_from_db.as_deref(),
        )
        .await
        {
            Ok(mut registry) => {
                {
                    let mut guard = state.lock().await;
                    guard.component_bindings = component_bindings;
                }
                if let Some(runtime) = local_runtime {
                    registry
                        .runtimes
                        .insert(selected_component_id.to_string(), runtime);
                    if !registry
                        .ordered_ids
                        .iter()
                        .any(|id| id == selected_component_id)
                    {
                        registry.ordered_ids.push(selected_component_id.to_string());
                    }
                }
                return Ok(registry);
            }
            Err(_err) if selected_is_local => {
                let mut runtimes = HashMap::new();
                runtimes.insert(
                    selected_component_id.to_string(),
                    local_runtime.expect("local runtime should exist for local component"),
                );
                return Ok(ComponentRuntimeRegistry {
                    runtimes,
                    ordered_ids: vec![selected_component_id.to_string()],
                });
            }
            Err(err) => return Err(err),
        }
    }

    if let Some(runtime) = local_runtime {
        let mut runtimes = HashMap::new();
        runtimes.insert(selected_component_id.to_string(), runtime);
        return Ok(ComponentRuntimeRegistry {
            runtimes,
            ordered_ids: vec![selected_component_id.to_string()],
        });
    }

    anyhow::bail!(
        "session required to load server component runtime '{}' for retranslate",
        selected_component_id
    );
}

fn is_language_pack_item(item: &crate::db::jobs::TranslationItem) -> bool {
    let object_type = item.object_type.trim().to_lowercase();
    let bl = item.business_line.trim().to_lowercase();
    object_type == "language_pack"
        || object_type == "site_string"
        || bl.ends_with("_i18n")
        || bl.ends_with("_strings")
}

#[allow(clippy::too_many_arguments)]
async fn sync_item_to_wp_for_review(
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    client: &Client,
    item: &crate::db::jobs::TranslationItem,
    wp_base: &str,
    token: &str,
    worker_config: &crate::types::WorkerConfig,
    route_secret: Option<&str>,
    log_file: &str,
    callback_sem: &Arc<tokio::sync::Semaphore>,
) -> anyhow::Result<()> {
    if is_language_pack_item(item) {
        crate::task_engine::pipeline::sync_i18n_item_to_wp(
            db,
            client,
            item.id,
            &item.translated_path,
            wp_base,
            token,
            worker_config,
            route_secret,
            log_file,
            callback_sem,
        )
        .await
        .map(|_| ())
    } else {
        crate::task_engine::pipeline::sync_item_to_wp(
            db,
            client,
            item.id,
            &item.translated_path,
            wp_base,
            token,
            worker_config,
            route_secret,
            log_file,
            callback_sem,
        )
        .await
    }
}

pub(super) async fn handle_item_resubmit(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id_str: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "item id must be an integer").await;
        }
    };

    let (db_arc, domain_token_bindings, client) = {
        let guard = state.lock().await;
        (
            std::sync::Arc::clone(&guard.db),
            guard.domain_token_bindings.clone(),
            guard.http_client.clone(),
        )
    };

    // 1. Fetch item from DB
    let item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item(&conn, id)
    };
    let item = match item {
        Some(i) => i,
        None => return write_not_found_response(socket, "NOT_FOUND", "item not found").await,
    };

    // 2. Validate status
    if item.status != "pending_review" && item.status != "translated" && item.status != "failed" {
        return write_error_response(
            socket,
            "INVALID_STATUS",
            &format!("cannot resubmit item with status '{}'", item.status),
        )
        .await;
    }

    // 3. Validate translated_path exists
    if item.translated_path.is_empty() || !std::path::Path::new(&item.translated_path).exists() {
        return write_error_response(
            socket,
            "MISSING_TRANSLATED_FILE",
            "translated file does not exist",
        )
        .await;
    }

    // 4. Resolve WP credentials from domain_token_bindings
    let wp_client_token_fallback = crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "");
    let wp_client_token = match crate::bindings::resolve_wp_client_token_for_domain(
        &item.domain,
        &domain_token_bindings,
        &wp_client_token_fallback,
    ) {
        Some(t) => t,
        None => {
            return write_error_response(
                socket,
                "MISSING_TOKEN",
                "no WP client token configured for this domain",
            )
            .await;
        }
    };
    let route_secret = match resolve_effective_route_secret_for_domain(
        state,
        &domain_token_bindings,
        &item.domain,
    )
    .await
    {
        Ok(Some(route_secret)) => route_secret,
        Ok(None) => {
            return write_error_response(
                socket,
                "MISSING_ROUTE_SECRET",
                "route_secret is missing for this domain. Configure it in Sites, or make sure server domains are refreshed and include route_secret.",
            )
            .await;
        }
        Err(err) => {
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            return write_error_response(
                socket,
                "ROUTE_SECRET_RESOLUTION_FAILED",
                &format!("{:#}", err),
            )
            .await;
        }
    };
    let domain_base = crate::bindings::normalize_domain_base(&item.domain);
    let wp_base = match crate::bindings::build_wp_base_url(&domain_base, &route_secret) {
        Some(wp_base) => wp_base,
        None => {
            return write_error_response(
                socket,
                "MISSING_ROUTE_SECRET",
                "route_secret is missing for this domain. Configure it in Sites, or make sure server domains are refreshed and include route_secret.",
            )
            .await;
        }
    };

    // 5. Transition to "translated" status if needed
    if item.status != "translated" {
        let conn = db_arc.lock().await;
        let _ = crate::db::jobs::update_item_status(&conn, id, "translated", None);
    }

    // 6. Build worker config and call sync_item_to_wp
    let device_id = {
        let guard = state.lock().await;
        guard.device_id.clone()
    };
    let worker_config = crate::worker::build_worker_config(&device_id);
    let log_file = crate::config::env_or("WPTSALL_LOG_FILE", crate::config::DEFAULT_LOG_FILE);
    let callback_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));

    let sync_result = sync_item_to_wp_for_review(
        &db_arc,
        &client,
        &item,
        &wp_base,
        &wp_client_token,
        &worker_config,
        Some(route_secret.as_str()),
        &log_file,
        &callback_sem,
    )
    .await;

    match sync_result {
        Ok(()) => {
            let payload = json!({
                "success": true,
                "data": { "item_id": id, "status": "done" }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            // Keep item on callback-recoverable lane.
            {
                let conn = db_arc.lock().await;
                let _ = crate::db::jobs::update_item_status(
                    &conn,
                    id,
                    "translated",
                    Some(&format!("{:#}", err)),
                );
            }
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            write_error_response(
                socket,
                "RESUBMIT_FAILED",
                &format!("sync to WP failed: {:#}", err),
            )
            .await
        }
    }
}

/// POST /api/items/:id/approve — approve a pending_review item, sync it to WP.
pub(super) async fn handle_item_approve(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id_str: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "item id must be an integer").await;
        }
    };

    let (db_arc, domain_token_bindings, client) = {
        let guard = state.lock().await;
        (
            std::sync::Arc::clone(&guard.db),
            guard.domain_token_bindings.clone(),
            guard.http_client.clone(),
        )
    };

    // 1. Fetch item from DB
    let item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item(&conn, id)
    };
    let item = match item {
        Some(i) => i,
        None => return write_not_found_response(socket, "NOT_FOUND", "item not found").await,
    };

    // 2. Validate status must be pending_review
    if item.status != "pending_review" {
        return write_error_response(
            socket,
            "INVALID_STATUS",
            &format!(
                "item status is '{}', expected 'pending_review'",
                item.status
            ),
        )
        .await;
    }

    // 3. Validate translated_path exists
    if item.translated_path.is_empty() || !std::path::Path::new(&item.translated_path).exists() {
        return write_error_response(
            socket,
            "MISSING_TRANSLATED_FILE",
            "translated file does not exist",
        )
        .await;
    }

    // 4. Resolve WP credentials
    let wp_client_token_fallback = crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "");
    let wp_client_token = match crate::bindings::resolve_wp_client_token_for_domain(
        &item.domain,
        &domain_token_bindings,
        &wp_client_token_fallback,
    ) {
        Some(t) => t,
        None => {
            return write_error_response(
                socket,
                "MISSING_TOKEN",
                "no WP client token configured for this domain",
            )
            .await;
        }
    };
    let route_secret = match resolve_effective_route_secret_for_domain(
        state,
        &domain_token_bindings,
        &item.domain,
    )
    .await
    {
        Ok(Some(route_secret)) => route_secret,
        Ok(None) => {
            return write_error_response(
                socket,
                "MISSING_ROUTE_SECRET",
                "route_secret is missing for this domain. Configure it in Sites, or make sure server domains are refreshed and include route_secret.",
            )
            .await;
        }
        Err(err) => {
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            return write_error_response(
                socket,
                "ROUTE_SECRET_RESOLUTION_FAILED",
                &format!("{:#}", err),
            )
            .await;
        }
    };
    let domain_base = crate::bindings::normalize_domain_base(&item.domain);
    let wp_base = match crate::bindings::build_wp_base_url(&domain_base, &route_secret) {
        Some(wp_base) => wp_base,
        None => {
            return write_error_response(
                socket,
                "MISSING_ROUTE_SECRET",
                "route_secret is missing for this domain. Configure it in Sites, or make sure server domains are refreshed and include route_secret.",
            )
            .await;
        }
    };

    // 5. Sync to WP
    let device_id = {
        let guard = state.lock().await;
        guard.device_id.clone()
    };
    let worker_config = crate::worker::build_worker_config(&device_id);
    let log_file = crate::config::env_or("WPTSALL_LOG_FILE", crate::config::DEFAULT_LOG_FILE);
    let callback_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));

    let sync_result = sync_item_to_wp_for_review(
        &db_arc,
        &client,
        &item,
        &wp_base,
        &wp_client_token,
        &worker_config,
        Some(route_secret.as_str()),
        &log_file,
        &callback_sem,
    )
    .await;

    match sync_result {
        Ok(()) => {
            let payload = json!({
                "success": true,
                "data": { "item_id": id, "status": "done" }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            {
                let conn = db_arc.lock().await;
                let _ = crate::db::jobs::update_item_status(
                    &conn,
                    id,
                    "pending_review",
                    Some(&format!("{:#}", err)),
                );
            }
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            write_error_response(
                socket,
                "APPROVE_FAILED",
                &format!("sync to WP failed: {:#}", err),
            )
            .await
        }
    }
}

/// POST /api/items/batch-approve — approve multiple pending_review items at once.
pub(super) async fn handle_items_batch_approve(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    struct BatchApproveRequest {
        ids: Vec<i64>,
    }

    let req: BatchApproveRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => {
            return write_error_response(socket, "INVALID_JSON", "expected { \"ids\": [1,2,...] }")
                .await;
        }
    };

    if req.ids.is_empty() {
        return write_error_response(socket, "EMPTY_IDS", "ids array is empty").await;
    }
    if req.ids.len() > 100 {
        return write_error_response(socket, "TOO_MANY", "max 100 items per batch").await;
    }

    let (db_arc, domain_token_bindings, client, device_id) = {
        let guard = state.lock().await;
        (
            std::sync::Arc::clone(&guard.db),
            guard.domain_token_bindings.clone(),
            guard.http_client.clone(),
            guard.device_id.clone(),
        )
    };

    let worker_config = crate::worker::build_worker_config(&device_id);
    let log_file = crate::config::env_or("WPTSALL_LOG_FILE", crate::config::DEFAULT_LOG_FILE);
    let callback_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    let wp_client_token_fallback = crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "");

    let mut approved = Vec::new();
    let mut failed = Vec::new();
    let mut skipped = Vec::new();

    for id in &req.ids {
        let item = {
            let conn = db_arc.lock().await;
            crate::db::jobs::get_item(&conn, *id)
        };
        let item = match item {
            Some(i) => i,
            None => {
                skipped.push(json!({"id": id, "reason": "not_found"}));
                continue;
            }
        };
        if item.status != "pending_review" {
            skipped.push(json!({"id": id, "reason": format!("status is {}", item.status)}));
            continue;
        }
        if item.translated_path.is_empty() || !std::path::Path::new(&item.translated_path).exists()
        {
            skipped.push(json!({"id": id, "reason": "missing_translated_file"}));
            continue;
        }

        let wp_client_token = match crate::bindings::resolve_wp_client_token_for_domain(
            &item.domain,
            &domain_token_bindings,
            &wp_client_token_fallback,
        ) {
            Some(t) => t,
            None => {
                skipped.push(json!({"id": id, "reason": "missing_token"}));
                continue;
            }
        };
        let route_secret = match resolve_effective_route_secret_for_domain(
            state,
            &domain_token_bindings,
            &item.domain,
        )
        .await
        {
            Ok(Some(route_secret)) => route_secret,
            Ok(None) => {
                skipped.push(json!({"id": id, "reason": "missing_route_secret"}));
                continue;
            }
            Err(err) => {
                failed.push(review_failed_item_payload(*id, &err));
                continue;
            }
        };
        let domain_base = crate::bindings::normalize_domain_base(&item.domain);
        let Some(wp_base) = crate::bindings::build_wp_base_url(&domain_base, &route_secret) else {
            skipped.push(json!({"id": id, "reason": "missing_route_secret"}));
            continue;
        };

        let sync_result = sync_item_to_wp_for_review(
            &db_arc,
            &client,
            &item,
            &wp_base,
            &wp_client_token,
            &worker_config,
            Some(route_secret.as_str()),
            &log_file,
            &callback_sem,
        )
        .await;

        match sync_result {
            Ok(()) => approved.push(*id),
            Err(err) => {
                {
                    let conn = db_arc.lock().await;
                    let _ = crate::db::jobs::update_item_status(
                        &conn,
                        *id,
                        "pending_review",
                        Some(&format!("{:#}", err)),
                    );
                }
                failed.push(review_failed_item_payload(*id, &err));
            }
        }
    }

    let payload = json!({
        "success": true,
        "data": {
            "approved": approved,
            "failed": failed,
            "skipped": skipped,
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}
