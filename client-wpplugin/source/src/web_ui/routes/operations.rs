use anyhow::Context;
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::types::{WebUiBatchTranslationRequest, WebUiLogsRequest, WebUiState};

use super::errors::{
    err_public, write_error_response, write_error_response_with_status, write_not_found_response,
};
use super::http::{parse_query_string, write_http_response};
use super::{
    load_local_components_runtime_doc, update_state_error,
    validate_editable_overrides_for_component_id, validate_local_component_runtime_ready_for_task,
};

pub(super) async fn handle_logs_recent(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    log_file: &str,
) -> anyhow::Result<()> {
    let req = if body.is_empty() {
        WebUiLogsRequest {
            limit: None,
            min_level: None,
            before_ts_ms: None,
            event_prefix: None,
        }
    } else {
        serde_json::from_slice::<WebUiLogsRequest>(body)
            .with_context(|| "invalid /api/logs/recent json payload")?
    };
    let limit = req.limit.unwrap_or(200).clamp(1, 1000);
    let min_level = req
        .min_level
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(crate::logging::level_to_u8);
    let event_prefix = req
        .event_prefix
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // FL-1 (Wave-2): flush the writer BEFORE reading. The log writer batches
    // info-level audit events until an 8 KB threshold (warn+ flushes
    // immediately), so without this the Logs page's auto-refresh and any
    // log-oracle read saw stale data — the tail of a burst (e.g. the final
    // job.finalized) sat invisible in the buffer until the NEXT event or
    // shutdown. Flushing on the read path costs one small write syscall per
    // poll and makes every read reflect every event written so far.
    crate::logging::flush_log();
    match crate::web_ui::read_log_lines_page(
        log_file,
        limit,
        min_level,
        req.before_ts_ms,
        event_prefix,
    ) {
        Ok(page) => {
            {
                let mut guard = state.lock().await;
                guard.last_error.clear();
                guard.last_event = "logs.recent_fetched".to_string();
                guard.updated_at = crate::logging::unix_ts();
            }
            let payload = json!({
                "success": true,
                "data": {
                    "limit": limit,
                    "min_level": req.min_level,
                    "before_ts_ms": req.before_ts_ms,
                    "event_prefix": req.event_prefix,
                    "next_before_ts_ms": page.next_before_ts_ms,
                    "has_more": page.has_more,
                    "lines": page.lines
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
        Err(err) => {
            update_state_error(state, &err, "logs.recent_fetch_failed").await;
            write_error_response(socket, "LOGS_FETCH_FAILED", &err_public(&err)).await
        }
    }
}

/// GET /api/logs/recent?limit=&min_level=&before_ts_ms=&event_prefix= — same payload as POST.
pub(super) async fn handle_logs_recent_get(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query: &str,
    log_file: &str,
) -> anyhow::Result<()> {
    let params = parse_query_string(query);
    let limit = params.get("limit").and_then(|v| v.parse::<usize>().ok());
    let min_level = params
        .get("min_level")
        .cloned()
        .filter(|s| !s.trim().is_empty());
    let before_ts_ms = params
        .get("before_ts_ms")
        .and_then(|v| v.parse::<u64>().ok());
    let event_prefix = params
        .get("event_prefix")
        .cloned()
        .filter(|s| !s.trim().is_empty());
    let body = serde_json::to_vec(&json!({
        "limit": limit,
        "min_level": min_level,
        "before_ts_ms": before_ts_ms,
        "event_prefix": event_prefix,
    }))
    .unwrap_or_default();
    handle_logs_recent(socket, state, &body, log_file).await
}

pub(super) async fn handle_translations_list(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let params = parse_query_string(query);
    let page: u32 = params.get("page").and_then(|v| v.parse().ok()).unwrap_or(1);
    let limit: u32 = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let domain = params.get("domain").filter(|v| !v.is_empty()).cloned();
    let status = params.get("status").filter(|v| !v.is_empty()).cloned();
    let search = params.get("search").filter(|v| !v.is_empty()).cloned();

    let query_params = crate::db::translations::TranslationQueryParams {
        page,
        limit,
        domain,
        status,
        search,
    };

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };

    let query_result = {
        let conn = db_arc.lock().await;
        crate::db::translations::query_translation_records(&conn, &query_params)
    };

    match query_result {
        Ok(list_result) => {
            let payload = json!({
                "success": true,
                "data": list_result
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
            write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "TRANSLATIONS_QUERY_FAILED",
                &err_public(&err),
            )
            .await
        }
    }
}

pub(super) async fn handle_translations_batch_retry(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiBatchTranslationRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => {
            return write_error_response(
                socket,
                "INVALID_BODY",
                "body must contain { ids: [...] }",
            )
            .await;
        }
    };
    if req.ids.is_empty() {
        return write_error_response(socket, "EMPTY_IDS", "ids array must not be empty").await;
    }
    if req.ids.len() > 100 {
        return write_error_response(socket, "TOO_MANY_IDS", "maximum 100 ids per request").await;
    }
    if req.ids.iter().any(|id| *id <= 0) {
        return write_error_response(
            socket,
            "INVALID_ID",
            "translation ids must be positive integers",
        )
        .await;
    }

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let result = {
        let conn = db_arc.lock().await;
        crate::db::translations::batch_retry_translations(&conn, &req.ids)
    };

    match result {
        Ok(queued) => {
            crate::logging::log_event_global(
                "info",
                "ops.translations_batch_retried",
                json!({ "count": queued, "requested": req.ids.len() }),
            );
            let payload = json!({
                "success": true,
                "data": { "queued": queued, "requested": req.ids.len() }
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
            write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "BATCH_RETRY_FAILED",
                &err_public(&err),
            )
            .await
        }
    }
}

pub(super) async fn handle_translations_batch_delete(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiBatchTranslationRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => {
            return write_error_response(
                socket,
                "INVALID_BODY",
                "body must contain { ids: [...] }",
            )
            .await;
        }
    };
    if req.ids.is_empty() {
        return write_error_response(socket, "EMPTY_IDS", "ids array must not be empty").await;
    }
    if req.ids.len() > 100 {
        return write_error_response(socket, "TOO_MANY_IDS", "maximum 100 ids per request").await;
    }
    if req.ids.iter().any(|id| *id <= 0) {
        return write_error_response(
            socket,
            "INVALID_ID",
            "translation ids must be positive integers",
        )
        .await;
    }

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let result = {
        let conn = db_arc.lock().await;
        crate::db::translations::batch_delete_translations(&conn, &req.ids)
    };

    match result {
        Ok(deleted) => {
            crate::logging::log_event_global(
                "warn",
                "ops.translations_batch_deleted",
                json!({ "count": deleted, "requested": req.ids.len() }),
            );
            let payload = json!({
                "success": true,
                "data": { "deleted": deleted, "requested": req.ids.len() }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => write_error_response(socket, "BATCH_DELETE_FAILED", &err_public(&err)).await,
    }
}

pub(super) async fn handle_discovery_tasks_list(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let tasks = {
        let conn = db_arc.lock().await;
        crate::db::discovery_tasks::list_discovery_tasks(&conn)
    };
    let tasks = match tasks {
        Ok(tasks) => tasks,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "DISCOVERY_TASKS_READ_FAILED",
                &err_public(&error),
            )
            .await
        }
    };
    let payload = json!({ "success": true, "data": { "items": tasks } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_discovery_tasks_bootstrap(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let (client, device_id, bindings_doc, db_arc) = {
        let guard = state.lock().await;
        (
            guard.http_client.clone(),
            guard.device_id.clone(),
            guard.domain_token_bindings.clone(),
            std::sync::Arc::clone(&guard.db),
        )
    };

    let mut binding_keys: Vec<String> = bindings_doc.domains.keys().cloned().collect();
    binding_keys.sort();

    let mut synced = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();

    for binding_key in binding_keys {
        let Some(entry) = bindings_doc.domains.get(&binding_key) else {
            continue;
        };

        let domain_base = crate::bindings::normalize_domain_base(&binding_key);
        let token = entry.wp_client_token.trim().to_string();
        let route_secret = entry.route_secret.trim().to_string();

        if domain_base.is_empty() || token.is_empty() {
            skipped.push(json!({
                "binding_key": binding_key,
                "reason": "missing_domain_or_token",
            }));
            continue;
        }

        let Some(wp_base) = crate::bindings::build_wp_base_url(&domain_base, &route_secret) else {
            skipped.push(json!({
                "binding_key": binding_key,
                "domain_base": domain_base,
                "reason": "missing_route_secret",
            }));
            continue;
        };

        let relations_url = format!("{}/site-relations", wp_base);
        let relation_ids = match crate::auth::wp_get_json_with_transport_and_secret::<
            crate::types::RelationsResponse,
        >(
            &client,
            &relations_url,
            &token,
            &device_id,
            &device_id,
            Some(route_secret.as_str()),
        )
        .await
        {
            Ok(relations) => relations
                .relations
                .into_iter()
                .map(|relation| relation.id)
                .filter(|id| *id > 0)
                .collect::<Vec<_>>(),
            Err(err) => {
                failed.push(json!({
                    "binding_key": binding_key,
                    "domain_base": domain_base,
                    "wp_base": wp_base,
                    "reason": "fetch_relations_failed",
                    "error": format!("{:#}", err),
                }));
                continue;
            }
        };

        let ensure_result = {
            let conn = db_arc.lock().await;
            crate::db::discovery_tasks::ensure_discovery_tasks(&conn, &wp_base, &relation_ids)
        };

        if let Err(err) = ensure_result {
            failed.push(json!({
                "binding_key": binding_key,
                "domain_base": domain_base,
                "wp_base": wp_base,
                "reason": "ensure_discovery_tasks_failed",
                "error": format!("{:#}", err),
            }));
            continue;
        }

        synced.push(json!({
            "binding_key": binding_key,
            "domain_base": domain_base,
            "wp_base": wp_base,
            "relation_ids": relation_ids,
        }));
    }

    if synced.is_empty() && !failed.is_empty() {
        return write_error_response(
            socket,
            "DISCOVERY_TASKS_BOOTSTRAP_FAILED",
            failed[0]
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("failed to bootstrap discovery tasks from WP relations"),
        )
        .await;
    }

    crate::logging::log_event_global(
        "info",
        "ops.discovery_bootstrapped",
        json!({ "synced": synced.len(), "skipped": skipped.len(), "failed": failed.len() }),
    );
    let payload = json!({
        "success": true,
        "data": {
            "synced": synced,
            "skipped": skipped,
            "failed": failed,
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

pub(super) async fn handle_discovery_task_update(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    id_str: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "task id must be an integer").await;
        }
    };
    let req: crate::types::WebUiUpdateDiscoveryTaskRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(err) => {
            return write_error_response(socket, "INVALID_BODY", &format!("invalid json: {}", err))
                .await;
        }
    };
    let db_req = crate::db::discovery_tasks::UpdateDiscoveryTaskRequest {
        concurrency: req.concurrency,
        batch_parallel: req.batch_parallel,
        per_page: req.per_page,
        retry_max: req.retry_max,
        timeout_secs: req.timeout_secs,
        enabled: req.enabled,
        include_resync: req.include_resync,
        selected_component_id: req.selected_component_id.clone(),
        effective_source_lang: req.effective_source_lang.clone(),
        effective_target_lang: req.effective_target_lang.clone(),
        editable_overrides: req.editable_overrides.clone(),
    };
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let current_task = {
        let conn = db_arc.lock().await;
        crate::db::discovery_tasks::get_discovery_task_by_id(&conn, id)
    };
    let current_task = match current_task {
        Ok(Some(task)) => task,
        Ok(None) => {
            return write_not_found_response(socket, "NOT_FOUND", "discovery task not found").await
        }
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "DISCOVERY_TASK_READ_FAILED",
                &err_public(&error),
            )
            .await
        }
    };
    let requested_component_id = req
        .selected_component_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let effective_component_id = if req.selected_component_id.is_some() {
        requested_component_id.clone()
    } else {
        current_task.selected_component_id.clone()
    };
    let effective_editable_overrides = req
        .editable_overrides
        .as_ref()
        .or(current_task.editable_overrides.as_ref());
    if (requested_component_id.is_some() || req.editable_overrides.is_some())
        && effective_component_id.is_some()
    {
        if let Err(err) = validate_local_component_runtime_ready_for_task(
            state,
            effective_component_id.as_deref().unwrap_or_default(),
            effective_editable_overrides,
        )
        .await
        {
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "INVALID_COMPONENT_ID",
                &err_public(&err),
            )
            .await;
        }
    } else if let Some(ref component_id) = requested_component_id {
        let local_doc = load_local_components_runtime_doc()?;
        if !local_doc.components.contains_key(component_id) {
            return write_error_response(
                socket,
                "INVALID_COMPONENT_ID",
                "selected component not found in local components",
            )
            .await;
        }
    }
    if let Err(err) = validate_editable_overrides_for_component_id(
        effective_component_id.as_deref(),
        effective_editable_overrides,
    ) {
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "INVALID_TASK_EDITABLE_OVERRIDES",
            &err_public(&err),
        )
        .await;
    }
    let result = {
        let conn = db_arc.lock().await;
        crate::db::discovery_tasks::update_discovery_task(&conn, id, &db_req)
    };
    match result {
        Ok(()) => {
            crate::logging::log_event_global(
                "info",
                "ops.discovery_task_updated",
                json!({ "id": id, "enabled": db_req.enabled }),
            );
            let payload = json!({ "success": true, "data": {} });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "UPDATE_FAILED",
                &err_public(&err),
            )
            .await
        }
    }
}

pub(super) async fn handle_stats_overview(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let (by_domain, by_status, daily) = {
        let conn = db_arc.lock().await;
        let by_domain = crate::db::translations::stats_by_domain(&conn);
        let by_status = crate::db::translations::stats_status_distribution(&conn);
        let daily = crate::db::translations::stats_daily(&conn, 30);
        (by_domain, by_status, daily)
    };
    let (by_domain, by_status, daily) = match (by_domain, by_status, daily) {
        (Ok(by_domain), Ok(by_status), Ok(daily)) => (by_domain, by_status, daily),
        _ => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "TRANSLATION_STATS_READ_FAILED",
                "Saved translation statistics could not be read; original evidence retained",
            )
            .await
        }
    };
    let payload = json!({
        "success": true,
        "data": {
            "by_domain": by_domain,
            "by_status": by_status,
            "daily": daily,
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
