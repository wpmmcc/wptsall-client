use anyhow::Context;
use reqwest::Client;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::types::*;

use super::errors::{
    err_public, maybe_write_upstream_api_error, review_failed_item_payload, write_error_response,
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

async fn claim_review_item(
    socket: &mut TcpStream,
    db: &Arc<Mutex<rusqlite::Connection>>,
    id: i64,
) -> anyhow::Result<Option<crate::db::unit_lock::UnitLease>> {
    match crate::db::unit_lock::UnitLease::item(db, id).await {
        Ok(lease) => Ok(Some(lease)),
        Err(error) => {
            let busy = error.to_string().contains("REVIEW_ITEM_BUSY");
            write_error_response_with_status(
                socket,
                if busy {
                    "409 Conflict"
                } else {
                    "500 Internal Server Error"
                },
                if busy {
                    "REVIEW_ITEM_BUSY"
                } else {
                    "REVIEW_CLAIM_FAILED"
                },
                &err_public(&error),
            )
            .await?;
            Ok(None)
        }
    }
}

async fn read_review_item(
    socket: &mut TcpStream,
    db: &Arc<Mutex<rusqlite::Connection>>,
    id: i64,
) -> anyhow::Result<Option<crate::db::jobs::TranslationItem>> {
    let result = crate::db::jobs::get_item_checked(&*db.lock().await, id);
    match result {
        Ok(Some(item)) => Ok(Some(item)),
        Ok(None) => {
            write_not_found_response(socket, "NOT_FOUND", "item not found").await?;
            Ok(None)
        }
        Err(error) => {
            write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "ITEM_READ_FAILED",
                &err_public(&error),
            )
            .await?;
            Ok(None)
        }
    }
}

fn review_has_unresolved_delivery(
    conn: &rusqlite::Connection,
    item: &crate::db::jobs::TranslationItem,
) -> anyhow::Result<bool> {
    if crate::task_engine::pipeline::language_pack::has_delivery(conn, item.id)? {
        return Ok(true);
    }
    let base = |url: &str| {
        url.split("/wp-json/")
            .next()
            .unwrap_or(url)
            .trim_end_matches('/')
            .to_string()
    };
    let object_type =
        crate::db::pending_callbacks::normalize_pending_object_type(&item.object_type)?;
    for pending in crate::db::pending_callbacks::list_all_pending_callbacks(conn)? {
        if pending.relation_id == item.relation_id
            && pending.object_id == item.wp_object_id
            && crate::db::pending_callbacks::normalize_pending_object_type(&pending.object_type)?
                == object_type
            && base(&pending.api_base_url) == base(&item.domain)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn review_is_editable(
    conn: &rusqlite::Connection,
    item: &crate::db::jobs::TranslationItem,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !review_has_unresolved_delivery(conn, item)?,
        "original saved delivery must be reconciled before changing intent"
    );
    Ok(())
}

fn review_is_mutable(
    conn: &rusqlite::Connection,
    item: &crate::db::jobs::TranslationItem,
) -> anyhow::Result<()> {
    review_is_editable(conn, item)?;
    anyhow::ensure!(
        !crate::db::review_attempts::unfinished(conn, item.id)?,
        "unfinished manual request must resume its original request_id before changing intent"
    );
    Ok(())
}

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
    let result = {
        let conn = db_arc.lock().await;
        (|| -> anyhow::Result<_> {
            let jobs = crate::db::jobs::list_jobs(
                &conn,
                domain.as_deref(),
                limit.clamp(1, 500),
                offset.max(0),
            )?;
            let progresses = jobs
                .iter()
                .map(|j| crate::db::jobs::get_job_progress(&conn, j.id))
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok((jobs, progresses))
        })()
    };
    let (jobs, progresses) = match result {
        Ok(result) => result,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "JOBS_READ_FAILED",
                &err_public(&error),
            )
            .await
        }
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
        crate::db::jobs::get_job_checked(&conn, id)
    };

    match job {
        Ok(Some(j)) => {
            let payload = json!({ "success": true, "data": j });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Ok(None) => write_not_found_response(socket, "NOT_FOUND", "job not found").await,
        Err(error) => {
            write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "JOB_READ_FAILED",
                &err_public(&error),
            )
            .await
        }
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
    let items = match items {
        Ok(items) => items,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "ITEMS_READ_FAILED",
                &err_public(&error),
            )
            .await
        }
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
    let Some(item) = read_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };

    let (manual_request, delivery_unresolved) = {
        let conn = db_arc.lock().await;
        (
            crate::db::review_attempts::pending_request(&conn, id)?,
            review_has_unresolved_delivery(&conn, &item)?,
        )
    };
    let saved_plan = match &manual_request {
        Some(request) => {
            crate::db::review_attempts::saved_plan(&*db_arc.lock().await, id, request)?
        }
        None if !std::path::Path::new(&item.raw_path).is_file() => {
            crate::db::review_attempts::retained_plan(&*db_arc.lock().await, id)?
        }
        None => None,
    };
    let contents = (|| -> anyhow::Result<(serde_json::Value, serde_json::Value)> {
        let read = |path: &str| -> anyhow::Result<serde_json::Value> {
            if path.is_empty() {
                return Ok(serde_json::Value::Null);
            }
            Ok(serde_json::from_str(
                &crate::bindings::load_encrypted_or_plain(std::path::Path::new(path))?,
            )?)
        };
        let raw = if let Some(saved) = saved_plan {
            let plan: crate::db::review_attempts::plan::ManualPlan = serde_json::from_value(saved)?;
            anyhow::ensure!(
                plan.format == "manual-plan-v1" && plan.content.is_object(),
                "saved manual source is damaged; retained"
            );
            plan.content
        } else {
            read(&item.raw_path)?
        };
        Ok((raw, read(&item.translated_path)?))
    })();
    let (raw_content, translated_content) = match contents {
        Ok(contents) => contents,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "409 Conflict",
                "REVIEW_ARTIFACT_UNRESOLVED",
                &err_public(&error),
            )
            .await
        }
    };
    let payload = json!({
        "success": true,
        "data": {
            "item": item,
            "raw": raw_content,
            "translated": translated_content,
            "manual_request_id": manual_request,
            "delivery_unresolved": delivery_unresolved,
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
    let Some(_review_lease) = claim_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };
    let item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item_checked(&conn, id)
    };
    let item = match item {
        Ok(Some(i)) => i,
        Ok(None) => return write_not_found_response(socket, "NOT_FOUND", "item not found").await,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "ITEM_READ_FAILED",
                &err_public(&error),
            )
            .await
        }
    };
    if let Err(error) = review_is_mutable(&*db_arc.lock().await, &item) {
        return write_error_response_with_status(
            socket,
            "409 Conflict",
            "REVIEW_DELIVERY_UNRESOLVED",
            &err_public(&error),
        )
        .await;
    }

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
    let existing_file =
        match crate::bindings::load_encrypted_or_plain(std::path::Path::new(&item.translated_path))
        {
            Ok(value) => value,
            Err(error) => {
                return write_error_response(
                    socket,
                    "RESULT_READ_FAILED",
                    &err_public(&error.into()),
                )
                .await
            }
        };
    let mut envelope: serde_json::Value =
        match serde_json::from_str::<serde_json::Value>(&existing_file) {
            Ok(value) if value.is_object() => value,
            _ => {
                return write_error_response(
                    socket,
                    "RESULT_DAMAGED",
                    "saved result is damaged; original retained",
                )
                .await
            }
        };
    // Snapshot the previous payload for change detection below.
    let old_payload = envelope
        .get("payload")
        .cloned()
        .unwrap_or_else(|| envelope.clone());
    let is_pack = crate::task_engine::pipeline::i18n_payload_value_has_entries(&old_payload);
    if is_pack {
        let valid = (|| -> anyhow::Result<()> {
            let original: Vec<crate::types::I18nCallbackEntry> =
                serde_json::from_value(old_payload["entries"].clone())?;
            let changed: Vec<crate::types::I18nCallbackEntry> =
                serde_json::from_value(content_to_write["entries"].clone())?;
            let original_ids = original
                .iter()
                .map(|entry| entry.entry_id)
                .collect::<std::collections::BTreeSet<_>>();
            let changed_ids = changed
                .iter()
                .map(|entry| entry.entry_id)
                .collect::<std::collections::BTreeSet<_>>();
            anyhow::ensure!(
                original_ids.len() == original.len()
                    && changed_ids.len() == changed.len()
                    && original_ids == changed_ids
                    && !changed.is_empty()
                    && changed
                        .iter()
                        .all(|entry| entry.entry_id > 0 && !entry.msgstr.trim().is_empty()),
                "language-pack review cannot change entry identities or erase translations"
            );
            for entry in changed {
                let source = original
                    .iter()
                    .find(|prior| prior.entry_id == entry.entry_id)
                    .context("original language-pack entry missing")?;
                crate::component_rt::content_safety::validate_interpolation_tokens(
                    &source.msgstr,
                    &entry.msgstr,
                )?;
            }
            Ok(())
        })();
        if let Err(error) = valid {
            return write_error_response_with_status(
                socket,
                "409 Conflict",
                "REVIEW_ENTRY_SCOPE_CHANGED",
                &err_public(&error),
            )
            .await;
        }
    }
    let old_payload_str = serde_json::to_string(&old_payload).unwrap_or_default();
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
            if !is_pack {
                let mut translated_fields = serde_json::Map::new();
                let mut translated_meta = serde_json::Map::new();
                // Scalar-only, string-normalized maps (CLI-BUG-03): the approval
                // pipeline deserializes envelope.payload back into
                // TranslationCallbackPayload where both translated_fields and
                // translated_meta are HashMap<String, String>. Arrays/objects are
                // raw envelope junk, not translations — skip them, and normalize
                // non-string scalars to their string form so the approve path can
                // never fail deserialization on a saved review edit.
                for (key, value) in content_obj {
                    let scalar = match value {
                        serde_json::Value::String(s) => serde_json::Value::String(s),
                        v if v.is_number() || v.is_boolean() => {
                            serde_json::Value::String(v.to_string())
                        }
                        _ => continue,
                    };
                    if key.starts_with('_') {
                        translated_meta.insert(key, scalar);
                    } else {
                        translated_fields.insert(key, scalar);
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
        }
    } else {
        envelope = content_to_write.clone();
    }

    // Review edits are a NEW submission of this translation. The WP callback
    // dedup binds client_task_id / Idempotency-Key to the first request body
    // hash; reusing the original identity after an edit is rejected with
    // 409 idempotency_conflict ("Same client_task_id with different body").
    // Mint a fresh identity whenever the payload actually changed so the
    // subsequent approve is processed as a new write-back attempt.
    let new_payload = envelope
        .get("payload")
        .cloned()
        .unwrap_or_else(|| envelope.clone());
    let new_payload_str = serde_json::to_string(&new_payload).unwrap_or_default();
    if new_payload_str != old_payload_str {
        let base = envelope
            .get("idempotency_key")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .or_else(|| {
                new_payload
                    .get("client_task_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "review-edit".to_string());
        let new_task_id = format!(
            "{}-edit-{}",
            if base.len() <= 80 {
                base.as_str()
            } else {
                "review"
            },
            uuid::Uuid::new_v4()
        );
        if let Some(obj) = envelope.as_object_mut() {
            obj.insert(
                "idempotency_key".to_string(),
                serde_json::Value::String(new_task_id.clone()),
            );
        }
        if let Some(payload_obj) = envelope.get_mut("payload").and_then(|v| v.as_object_mut()) {
            payload_obj.insert(
                "client_task_id".to_string(),
                serde_json::Value::String(new_task_id),
            );
        }
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

    let commit = (|| -> anyhow::Result<_> {
        let new_identity = envelope
            .get("payload")
            .and_then(|payload| payload.get("client_task_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&item.client_task_id);
        let next_path = format!(
            "{}.edit-{}.json",
            item.translated_path,
            uuid::Uuid::new_v4()
        );
        let encoded = crate::bindings::encrypt_for_save(&json_str)?;
        crate::bindings::atomic_file::install_new(std::path::Path::new(&next_path), &encoded)?;
        Ok((next_path, new_identity.to_string()))
    })();
    let (next_path, new_identity) = match commit {
        Ok(value) => value,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "RESULT_SAVE_FAILED",
                &err_public(&error),
            )
            .await
        }
    };
    let projection = {
        let mut conn = db_arc.lock().await;
        (|| -> anyhow::Result<()> {
            let tx = conn.savepoint()?;
            _review_lease.assert_item(&tx, &db_arc, id)?;
            anyhow::ensure!(tx.execute(
                "UPDATE translation_items SET translated_path=?1, client_task_id=?2, updated_at=?3
                 WHERE id=?4 AND status='pending_review' AND translated_path=?5 AND client_task_id=?6",
                rusqlite::params![next_path, new_identity, crate::logging::unix_ts(), id,
                    item.translated_path, item.client_task_id],
            )? == 1, "edited result projection was not committed; original retained");
            tx.commit()?;
            Ok(())
        })()
    };
    if let Err(error) = projection {
        return write_error_response_with_status(
            socket,
            "500 Internal Server Error",
            "RESULT_SAVE_FAILED",
            &err_public(&error),
        )
        .await;
    }

    crate::logging::log_event_global(
        "info",
        "review.item_translated_saved",
        json!({ "item_id": id, "job_id": item.job_id, "domain": item.domain }),
    );
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
    let Some(_review_lease) = claim_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };
    let item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item_checked(&conn, id)
    };
    let item = match item {
        Ok(Some(i)) => i,
        Ok(None) => return write_not_found_response(socket, "NOT_FOUND", "item not found").await,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "ITEM_READ_FAILED",
                &err_public(&error),
            )
            .await
        }
    };
    if let Err(error) = review_is_editable(&*db_arc.lock().await, &item) {
        return write_error_response_with_status(
            socket,
            "409 Conflict",
            "REVIEW_DELIVERY_UNRESOLVED",
            &err_public(&error),
        )
        .await;
    }

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
    if effective_selected_component_id != item.selected_component_id
        || effective_source_lang != item.effective_source_lang
        || effective_target_lang != item.effective_target_lang
        || effective_editable_overrides != item.editable_overrides
    {
        if let Err(error) = review_is_mutable(&*db_arc.lock().await, &item) {
            return write_error_response_with_status(
                socket,
                "409 Conflict",
                "MANUAL_REQUEST_UNRESOLVED",
                &err_public(&error),
            )
            .await;
        }
    }
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
                &err_public(&err),
            )
            .await;
        }
    } else if let Some(ref component_id) = selected_component_id {
        let local_doc = load_local_components_runtime_doc()?;
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
            &err_public(&err),
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

    crate::logging::log_event_global(
        "info",
        "review.item_override_saved",
        json!({
            "item_id": id,
            "job_id": item.job_id,
            "component_id": effective_selected_component_id,
        }),
    );
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualRetranslateRequest {
    request_id: String,
    #[serde(default)]
    resume_only: bool,
}

pub(super) async fn handle_item_retranslate(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id_str: &str,
    body: &[u8],
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

    let Some(review_lease) = claim_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };
    let Some(item) = read_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };
    let body: ManualRetranslateRequest = match serde_json::from_slice(body) {
        Ok(body) => body,
        Err(_) => {
            return write_error_response(
                socket,
                "INVALID_REQUEST_BODY",
                "manual retranslation requires a request_id and boolean resume_only",
            )
            .await
        }
    };
    let request = body.request_id;
    if crate::db::review_attempts::validate_request(&request).is_err() {
        return write_error_response(
            socket,
            "INVALID_REQUEST_ID",
            "manual retranslation requires a canonical request_id",
        )
        .await;
    }
    let replay =
        crate::db::review_attempts::recover_complete_file(&db_arc, &review_lease, id, &request)
            .await;
    match replay {
        Ok(Some(receipt)) => {
            let current = crate::db::jobs::get_item_checked(&*db_arc.lock().await, id)?
                .ok_or_else(|| anyhow::anyhow!("manual item disappeared; retained"))?;
            return write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&json!({"success":true,"data":{
                    "item_id":id,"status":current.status,"component_id":receipt.component_id,
                    "source_lang":receipt.source_lang,"target_lang":receipt.target_lang,
                    "translated_path":receipt.translated_path,"request_id":request,"replayed":true,
                }}))?,
            )
            .await;
        }
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "409 Conflict",
                "MANUAL_RESULT_UNRESOLVED",
                &err_public(&error),
            )
            .await
        }
        Ok(None) => {}
    }
    if let Err(error) = review_is_editable(&*db_arc.lock().await, &item) {
        return write_error_response_with_status(
            socket,
            "409 Conflict",
            "REVIEW_DELIVERY_UNRESOLVED",
            &err_public(&error),
        )
        .await;
    }

    if !matches!(
        item.status.as_str(),
        "pending_review" | "rejected" | "failed" | "translated"
    ) {
        return write_error_response(
            socket,
            "INVALID_STATUS",
            "item cannot enter retranslate in this state",
        )
        .await;
    }
    let selected_component_id = item
        .selected_component_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(item.component_id.as_str())
        .to_string();
    let saved_plan =
        match crate::db::review_attempts::saved_plan(&*db_arc.lock().await, id, &request) {
            Ok(plan) => plan,
            Err(error) => {
                return write_error_response_with_status(
                    socket,
                    "409 Conflict",
                    "MANUAL_PLAN_UNRESOLVED",
                    &err_public(&error),
                )
                .await
            }
        };
    let plan_value = if let Some(saved) = saved_plan {
        saved
    } else {
        if body.resume_only {
            return write_error_response_with_status(socket,"409 Conflict","MANUAL_REQUEST_UNKNOWN",
            "No saved request exists for this item and database. Resume refused; no new translation was started.").await;
        }
        if item.raw_path.is_empty() || !std::path::Path::new(&item.raw_path).exists() {
            return write_error_response(
                socket,
                "MISSING_RAW_FILE",
                "raw file does not exist; cannot retranslate",
            )
            .await;
        }

        let mut complete_data: serde_json::Value =
            match crate::bindings::load_encrypted_or_plain(std::path::Path::new(&item.raw_path)) {
                Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
                    Ok(value) if value.is_object() => value,
                    _ => {
                        return write_error_response(
                            socket,
                            "RAW_DAMAGED",
                            "saved source is damaged; original retained",
                        )
                        .await
                    }
                },
                Err(err) => {
                    return write_error_response(
                        socket,
                        "RAW_READ_FAILED",
                        &format!("failed to read raw file: {}", err),
                    )
                    .await;
                }
            };
        if is_language_pack_item(&item) && complete_data["entries"].is_array() {
            let saved: serde_json::Value = match crate::bindings::load_encrypted_or_plain(
                std::path::Path::new(&item.translated_path),
            )
            .and_then(|text| Ok(serde_json::from_str(&text)?))
            {
                Ok(saved) => saved,
                Err(error) => {
                    return write_error_response(
                        socket,
                        "MANUAL_SOURCE_UNRESOLVED",
                        &err_public(&error),
                    )
                    .await;
                }
            };
            if let Err(error) =
                crate::task_engine::pipeline::language_pack::read_envelope(&saved, &item)
            {
                return write_error_response(
                    socket,
                    "MANUAL_SOURCE_UNRESOLVED",
                    &err_public(&error),
                )
                .await;
            }
            complete_data["__manual_i18n_envelope"] = saved;
            let worker = crate::worker::build_worker_config(&device_id);
            complete_data["__manual_i18n_limits"] = json!({
                "max_input_chars":worker.default_max_input_chars,
                "split_strategy":worker.default_split_strategy,
            });
        }

        let registry = match build_runtime_registry_for_retranslate(
            state,
            &selected_component_id,
            item.editable_overrides.as_ref(),
            &crate::config::log_file_path(),
        )
        .await
        {
            Ok(registry) => registry,
            Err(err) => {
                return write_error_response(
                    socket,
                    "COMPONENT_RUNTIME_BUILD_FAILED",
                    &err_public(&err),
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
                    &err_public(&err),
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

        let relations =
            match crate::auth::wp_get_json_with_transport_and_secret::<RelationsResponse>(
                &client,
                &relations_url,
                &wp_client_token,
                &worker_id,
                &worker_config.device_id,
                Some(route_secret.as_str()),
            )
            .await
            {
                Ok(relations) => relations,
                Err(err) => {
                    if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                        return response;
                    }
                    return write_error_response(
                        socket,
                        "FETCH_RELATIONS_FAILED",
                        &err_public(&err),
                    )
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

        let rules = if complete_data.get("__manual_i18n_envelope").is_some() {
            Vec::new()
        } else {
            match crate::auth::wp_get_json_with_transport_and_secret::<RulesResponse>(
                &client,
                &rules_url,
                &wp_client_token,
                &worker_id,
                &worker_config.device_id,
                Some(route_secret.as_str()),
            )
            .await
            {
                Ok(rules) => rules.rules,
                Err(err) => {
                    if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                        return response;
                    }
                    return write_error_response(socket, "FETCH_RULES_FAILED", &err_public(&err))
                        .await;
                }
            }
        };
        let profiles = if registry
            .runtimes
            .values()
            .any(|runtime| runtime.proxy_profile_id.is_some())
        {
            crate::bindings::load_proxy_profiles(&crate::config::proxy_profiles_file())?.profiles
        } else {
            HashMap::new()
        };
        let proxy_profiles =
            crate::db::review_attempts::plan::ManualPlan::freeze_proxies(&registry, &profiles)?;
        serde_json::to_value(crate::db::review_attempts::plan::ManualPlan {
            format: "manual-plan-v1".into(),
            wp_base,
            content: complete_data,
            relation,
            rules,
            runtimes: crate::db::review_attempts::plan::ManualPlan::freeze_runtimes(&registry)
                .await?,
            ordered_ids: registry.ordered_ids,
            rule_bindings,
            task_type_bindings,
            proxy_profiles,
        })?
    };

    let plan: crate::db::review_attempts::plan::ManualPlan =
        match serde_json::from_value(plan_value.clone()) {
            Ok(plan) => plan,
            Err(error) => {
                return write_error_response_with_status(
                    socket,
                    "409 Conflict",
                    "MANUAL_PLAN_UNRESOLVED",
                    &err_public(&error.into()),
                )
                .await
            }
        };
    let wp_base = plan.wp_base.clone();
    let proxy_pool = match plan.proxy_pool() {
        Ok(pool) => pool,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "409 Conflict",
                "MANUAL_PROXY_UNRESOLVED",
                &err_public(&error),
            )
            .await
        }
    };
    let relation = plan.relation.clone();
    let rules = plan.rules.clone();
    let rule_bindings = plan.rule_bindings.clone();
    let task_type_bindings = plan.task_type_bindings.clone();
    let pack_content = plan.content.clone();
    let content_item = ContentItem {
        object_type: item.object_type.clone(),
        subtype: item.wp_object_subtype.clone(),
        object_id: item.wp_object_id,
        needs_resync: false,
        mapping_id: None,
        complete_data: plan.content.clone(),
    };
    let registry = plan.registry(&client, db_arc.clone())?;
    let worker_config = crate::worker::build_worker_config(&device_id);

    let async_scope = {
        let conn = db_arc.lock().await;
        crate::db::review_attempts::scope(
            &conn,
            &db_arc,
            &review_lease,
            &item,
            &plan_value,
            &request,
        )?
    };
    if is_language_pack_item(&item) && pack_content.get("__manual_i18n_envelope").is_some() {
        let envelope = match crate::task_engine::pipeline::language_pack::retranslate(
            &proxy_pool,
            &registry,
            &selected_component_id,
            &item,
            &pack_content,
            &relation,
            &async_scope,
        )
        .await
        {
            Ok(envelope) => envelope,
            Err(error) => {
                return write_error_response_with_status(
                    socket,
                    "409 Conflict",
                    "MANUAL_FIELDS_UNRESOLVED",
                    &err_public(&error),
                )
                .await;
            }
        };
        let base_path = crate::task_engine::pipeline::build_translated_path(
            &item.raw_path,
            &crate::config::env_or("WPTSALL_DATA_DIR", crate::config::DEFAULT_DATA_DIR),
            &crate::task_engine::pipeline::sanitize_domain_key(&item.domain),
        );
        let path = format!("{base_path}.review-{request}.json");
        if let Err(error) = crate::task_engine::pipeline::language_pack::persist_manual_result(
            &review_lease,
            &db_arc,
            item.id,
            &envelope,
            &path,
            &async_scope,
        )
        .await
        {
            return write_error_response(socket, "PERSIST_TRANSLATED_FAILED", &err_public(&error))
                .await;
        }
        return write_http_response(
            socket,
            "200 OK",
            "application/json",
            &serde_json::to_vec(&json!({"success":true,"data":{
                "item_id":id,"status":"pending_review","request_id":request,
                "component_id":selected_component_id,
                "source_lang":relation.source_lang,"target_lang":relation.target_lang,
                "translated_path":path,
            }}))?,
        )
        .await;
    }
    let result = crate::task_engine::pipeline::translate_item_fields_with_trace_using_proxy(
        &client,
        Some(&proxy_pool),
        &wp_base,
        &content_item,
        &relation,
        &rules,
        Some(&registry),
        &selected_component_id,
        &[],
        Some(&task_type_bindings),
        Some(&rule_bindings),
        &worker_config,
        &crate::config::log_file_path(),
        None,
        Some(async_scope.clone()),
    )
    .await;

    let (payload, idempotency_key) = match result {
        Ok(Some(trace)) => (trace.payload, trace.idempotency_key),
        Ok(None) => {
            return write_error_response(
                socket,
                "NO_TRANSLATABLE_FIELDS",
                "item has no translatable fields after retranslate",
            )
            .await;
        }
        Err(err) => {
            return write_error_response(socket, "RETRANSLATE_FAILED", &err_public(&err)).await;
        }
    };

    if payload
        .field_results
        .iter()
        .any(|field| field.status == "failed")
    {
        return write_error_response_with_status(socket,"409 Conflict","MANUAL_FIELDS_UNRESOLVED",
            "manual fields remain unresolved; completed units retained, resume the original request_id").await;
    }

    let base_path = crate::task_engine::pipeline::build_translated_path(
        &item.raw_path,
        &crate::config::env_or("WPTSALL_DATA_DIR", crate::config::DEFAULT_DATA_DIR),
        &crate::task_engine::pipeline::sanitize_domain_key(&item.domain),
    );
    let generation = async_scope
        .source_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.get("manual_generation"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("manual generation missing before result save"))?;
    let translated_path = format!("{base_path}.review-{generation}.json");

    if let Err(err) = crate::task_engine::pipeline::persist_translated_claimed(
        &review_lease,
        &db_arc,
        item.id,
        &payload,
        &idempotency_key,
        None,
        &translated_path,
        &crate::config::log_file_path(),
        Some(&async_scope),
    )
    .await
    {
        return write_error_response(socket, "PERSIST_TRANSLATED_FAILED", &err_public(&err)).await;
    }

    crate::logging::log_event_global(
        "info",
        "review.item_retranslated",
        json!({
            "item_id": item.id,
            "job_id": item.job_id,
            "domain": item.domain,
            "component_id": selected_component_id,
        }),
    );
    let payload = json!({
        "success": true,
        "data": {
            "item_id": item.id,
            "status": "pending_review",
            "request_id": request,
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

    let local_doc = load_local_components_runtime_doc()?;
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
            Err(err) if err.is::<crate::component_rt::loader::RuntimeConfigurationFault>() => {
                return Err(err)
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

/// 批 O6 复栈 (B 档净室定谳): payload-shape probe mirroring the WP plugin's
/// callback routing (class-client-data-rest-controller.php — the *_i18n
/// family "sometimes classifies CPT/config objects as *_i18n but still
/// submits content-shaped payloads (translated_fields/meta) without
/// entries" and accepts them via the content write-back path). The review
/// sync must route the same way: a business_line of config_i18n with a
/// post/option object translates FIELD maps, and its translated file never
/// carries po-style `entries` — the old name-based routing deserialized it
/// into I18nCallbackPayload and hard-400ed the approve with
/// "missing field `entries`". Envelope handling matches
/// sync_i18n_item_to_wp (payload nested or bare); unreadable/unparseable
/// files keep the legacy i18n routing so genuine packs surface their own
/// errors unchanged.
///
/// Batch P1 (发布批): the probe now lives in pipeline.rs
/// (`translated_payload_has_i18n_entries`) so the review path and the
/// sync_i18n_item_to_wp shape fallback share one canonical implementation.
fn translated_payload_has_i18n_entries(translated_path: &str) -> bool {
    crate::task_engine::pipeline::translated_payload_has_i18n_entries(translated_path)
}

#[allow(clippy::too_many_arguments)]
async fn sync_item_to_wp_for_review(
    lease: &crate::db::unit_lock::UnitLease,
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
    if is_language_pack_item(item) && translated_payload_has_i18n_entries(&item.translated_path) {
        crate::task_engine::pipeline::sync_i18n_item_to_wp_claimed(
            lease,
            db,
            client,
            item.id,
            &item.translated_path,
            wp_base,
            token,
            worker_config,
            log_file,
            callback_sem,
        )
        .await
        .map(|_| ())
    } else {
        crate::task_engine::pipeline::sync_item_to_wp_claimed(
            lease,
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

    let Some(review_lease) = claim_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };
    // 1. Fetch item from DB
    let Some(item) = read_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };

    if crate::db::review_attempts::unfinished(&*db_arc.lock().await, id)? {
        return write_error_response_with_status(
            socket,
            "409 Conflict",
            "REVIEW_REQUEST_UNRESOLVED",
            "unfinished manual request must resume before callback delivery",
        )
        .await;
    }

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
                &err_public(&err),
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
        review_lease
            .mutate_item(&db_arc, id, |tx| {
                crate::db::jobs::update_item_status(tx, id, "translated", None)
            })
            .await?;
    }

    // 6. Build worker config and call sync_item_to_wp
    let device_id = {
        let guard = state.lock().await;
        guard.device_id.clone()
    };
    let worker_config = crate::worker::build_worker_config(&device_id);
    let log_file = crate::config::log_file_path();
    let callback_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));

    let sync_result = sync_item_to_wp_for_review(
        &review_lease,
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
            crate::logging::log_event_global(
                "info",
                "review.item_resubmitted",
                json!({ "item_id": id, "job_id": item.job_id, "domain": item.domain }),
            );
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
                review_lease
                    .mutate_item(&db_arc, id, |tx| {
                        crate::db::jobs::update_item_status(
                            tx,
                            id,
                            "translated",
                            Some(&format!("{:#}", err)),
                        )
                    })
                    .await?;
            }
            crate::logging::log_event_global(
                "warn",
                "review.item_resubmit_failed",
                json!({
                    "item_id": id,
                    "job_id": item.job_id,
                    "domain": item.domain,
                    "error": format!("{:#}", err),
                }),
            );
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

    let Some(review_lease) = claim_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };
    // 1. Fetch item from DB
    let Some(item) = read_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };

    if crate::db::review_attempts::unfinished(&*db_arc.lock().await, id)? {
        return write_error_response_with_status(
            socket,
            "409 Conflict",
            "REVIEW_REQUEST_UNRESOLVED",
            "unfinished manual request must resume before approval",
        )
        .await;
    }

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
                &err_public(&err),
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
    let log_file = crate::config::log_file_path();
    let callback_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(1));

    let sync_result = sync_item_to_wp_for_review(
        &review_lease,
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
            crate::logging::log_event_global(
                "info",
                "review.item_approved",
                json!({
                    "item_id": id,
                    "job_id": item.job_id,
                    "domain": item.domain,
                    "component_id": item.component_id,
                }),
            );
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
                review_lease
                    .mutate_item(&db_arc, id, |tx| {
                        crate::db::jobs::update_item_status(
                            tx,
                            id,
                            "pending_review",
                            Some(&format!("{:#}", err)),
                        )
                    })
                    .await?;
            }
            crate::logging::log_event_global(
                "warn",
                "review.item_approve_failed",
                json!({
                    "item_id": id,
                    "job_id": item.job_id,
                    "domain": item.domain,
                    "error": format!("{:#}", err),
                }),
            );
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

/// GET /api/items/pending-review — aggregated pending-review inbox (+ optional count-only).
pub(super) async fn handle_items_pending_review(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let params = parse_query_string(query);
    let limit: i64 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let count_only = params
        .get("count_only")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let result = {
        let conn = db_arc.lock().await;
        (|| -> anyhow::Result<_> {
            let total = crate::db::jobs::count_pending_review_items(&conn)?;
            if count_only {
                Ok((total, Vec::new()))
            } else {
                let items = crate::db::jobs::list_pending_review_items(&conn, limit)?;
                Ok((total, items))
            }
        })()
    };
    let (total, items) = match result {
        Ok(result) => result,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "REVIEW_READ_FAILED",
                &err_public(&error),
            )
            .await
        }
    };

    let payload = json!({
        "success": true,
        "data": {
            "total": total,
            "items": items,
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

/// POST /api/items/batch-reject — reject multiple pending_review (or translated) items.
pub(super) async fn handle_items_batch_reject(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    struct BatchRejectRequest {
        ids: Vec<i64>,
        reason: Option<String>,
    }

    let req: BatchRejectRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => {
            return write_error_response(
                socket,
                "INVALID_JSON",
                "expected { \"ids\": [1,2,...], \"reason\": \"...\" }",
            )
            .await;
        }
    };

    if req.ids.is_empty() {
        return write_error_response(socket, "EMPTY_IDS", "ids array is empty").await;
    }
    if req.ids.len() > 100 {
        return write_error_response(socket, "TOO_MANY", "max 100 items per batch").await;
    }

    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Rejected by reviewer")
        .to_string();

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };

    let mut rejected = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();

    for id in &req.ids {
        let review_lease = match crate::db::unit_lock::UnitLease::item(&db_arc, *id).await {
            Ok(lease) => lease,
            Err(error) => {
                failed.push(review_failed_item_payload(*id, &error));
                continue;
            }
        };
        let item = {
            let conn = db_arc.lock().await;
            crate::db::jobs::get_item_checked(&conn, *id)
        };
        let item = match item {
            Ok(Some(i)) => i,
            Err(error) => {
                failed.push(review_failed_item_payload(*id, &error));
                continue;
            }
            Ok(None) => {
                skipped.push(json!({"id": id, "reason": "not_found"}));
                continue;
            }
        };
        if item.status != "pending_review" && item.status != "translated" {
            skipped.push(json!({"id": id, "reason": format!("status is {}", item.status)}));
            continue;
        }
        let rejected_result = review_lease
            .mutate_item(&db_arc, *id, |tx| {
                review_is_mutable(tx, &item)?;
                crate::db::jobs::update_item_status(tx, *id, "rejected", Some(&reason))
            })
            .await;
        match rejected_result {
            Ok(()) => rejected.push(*id),
            Err(e) => {
                failed.push(json!({
                    "id": id,
                    "message": format!("{e:#}"),
                }));
            }
        }
    }

    let payload = json!({
        "success": true,
        "data": {
            "rejected": rejected,
            "failed": failed,
            "skipped": skipped,
        }
    });
    crate::logging::log_event_global(
        "info",
        "review.batch_rejected",
        json!({
            "rejected": rejected.len(),
            "skipped": skipped.len(),
            "failed": failed.len(),
            "reason": reason,
        }),
    );
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
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
    let log_file = crate::config::log_file_path();
    let callback_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    let wp_client_token_fallback = crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "");

    let mut approved = Vec::new();
    let mut failed = Vec::new();
    let mut skipped = Vec::new();

    for id in &req.ids {
        let review_lease = match crate::db::unit_lock::UnitLease::item(&db_arc, *id).await {
            Ok(lease) => lease,
            Err(error) => {
                failed.push(review_failed_item_payload(*id, &error));
                continue;
            }
        };
        let item = {
            let conn = db_arc.lock().await;
            crate::db::jobs::get_item_checked(&conn, *id)
        };
        let item = match item {
            Ok(Some(i)) => i,
            Ok(None) => {
                skipped.push(json!({"id": id, "reason": "not_found"}));
                continue;
            }
            Err(error) => {
                failed.push(review_failed_item_payload(*id, &error));
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
            &review_lease,
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
    crate::logging::log_event_global(
        "info",
        "review.batch_approved",
        json!({
            "approved": approved.len(),
            "skipped": skipped.len(),
            "failed": failed.len(),
        }),
    );
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

/// POST /api/items/:id/reject — reject a pending_review item with a reason.
pub(super) async fn handle_item_reject(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id_str: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(socket, "INVALID_ID", "item id must be an integer").await;
        }
    };

    #[derive(serde::Deserialize)]
    struct RejectRequest {
        reason: Option<String>,
    }

    let req: RejectRequest = if body.is_empty() {
        RejectRequest { reason: None }
    } else {
        serde_json::from_slice(body).unwrap_or(RejectRequest { reason: None })
    };

    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Rejected by reviewer");

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };

    let Some(review_lease) = claim_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };
    // 1. Fetch item from DB
    let Some(item) = read_review_item(socket, &db_arc, id).await? else {
        return Ok(());
    };
    if let Err(error) = review_is_mutable(&*db_arc.lock().await, &item) {
        return write_error_response_with_status(
            socket,
            "409 Conflict",
            "REVIEW_DELIVERY_UNRESOLVED",
            &err_public(&error),
        )
        .await;
    }

    // 2. Validate status must be pending_review or translated
    if item.status != "pending_review" && item.status != "translated" {
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

    // 3. Update status to rejected with reason
    {
        if let Err(e) = review_lease
            .mutate_item(&db_arc, id, |tx| {
                crate::db::jobs::update_item_status(tx, id, "rejected", Some(reason))
            })
            .await
        {
            return write_error_response(
                socket,
                "DB_ERROR",
                &format!("failed to update item status: {e:#}"),
            )
            .await;
        }
    }

    eprintln!("[review] Translation item {} rejected: {:?}", id, reason);
    crate::logging::log_event_global(
        "info",
        "review.item_rejected",
        json!({
            "item_id": id,
            "job_id": item.job_id,
            "domain": item.domain,
            "reason": reason,
        }),
    );

    let payload = json!({
        "success": true,
        "data": {
            "item_id": id,
            "status": "rejected",
            "reason": reason
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
