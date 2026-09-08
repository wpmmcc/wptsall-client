use std::sync::Arc;

use serde_json::json;
use tokio::sync::{Mutex, Semaphore};

use super::*;
use crate::task_engine::submitter::upload_pending_media;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TranslateContentOutcome {
    Completed,
    NoChanges,
    PendingReview,
    PendingCallback,
}

#[derive(Debug, Clone, Default)]
pub(super) struct TranslationExecutionSummary {
    pub component_ids: Vec<String>,
    pub media_mappings_count: i32,
    pub failed_fields_count: i32,
    pub primary_failure_reason: Option<String>,
}

fn build_translation_execution_summary(
    payload: &TranslationCallbackPayload,
    component_ids: Vec<String>,
) -> TranslationExecutionSummary {
    let failed_fields: Vec<&CallbackFieldResult> = payload
        .field_results
        .iter()
        .filter(|result| result.status == "failed")
        .collect();
    let primary_failure_reason = failed_fields.iter().find_map(|result| {
        [result.fallback_reason.trim(), result.detail.trim()]
            .into_iter()
            .find(|value| !value.is_empty())
            .map(str::to_string)
    });

    TranslationExecutionSummary {
        component_ids,
        media_mappings_count: i32::try_from(payload.media_mappings.len()).unwrap_or(i32::MAX),
        failed_fields_count: i32::try_from(failed_fields.len()).unwrap_or(i32::MAX),
        primary_failure_reason,
    }
}

/// Translate a single content item (owned parameters for use with tokio::spawn).
///
/// Returns `Ok(true)` if translated fields were submitted, `Ok(false)` if
/// there was nothing to translate, `Err` on failure.
#[allow(clippy::too_many_arguments)]
pub(super) async fn translate_content_item_owned(
    client: Client,
    wp_base: String,
    token: String,
    log_file: String,
    item: ContentItem,
    relation: DiscoveredRelation,
    rules: Arc<Vec<DiscoveredRule>>,
    component_registry: Option<Arc<ComponentRuntimeRegistry>>,
    proxy_pool: Option<Arc<ProxyClientPool>>,
    component_id_override: String,
    component_prefer_ids: Vec<String>,
    task_type_component_bindings: Option<Arc<TaskTypeComponentBindingsDoc>>,
    rule_component_bindings: Option<Arc<RuleComponentBindingsDoc>>,
    worker_config: WorkerConfig,
    route_secret: Option<String>,
    pending_store: Arc<Mutex<PendingCallbackStore>>,
    db: Option<Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
    callback_sem: Arc<Semaphore>,
    global_translation_sem: Option<Arc<Semaphore>>,
    global_callback_sem: Option<Arc<Semaphore>>,
    adaptive: Arc<AdaptiveRateControl>,
    job_id: Option<i64>,
    task_params: DiscoveryTaskParams,
) -> anyhow::Result<bool> {
    let object_id = u64::try_from(item.object_id).unwrap_or(0);
    let relation_id = u64::try_from(relation.id).unwrap_or(0);
    let object_id_i64 = item.object_id;
    let relation_id_i64 = relation.id;
    let object_type_key = normalize_object_type_key(&item.object_type);

    if item.needs_resync {
        let cleared = if let Some(ref db_arc) = db {
            let conn = db_arc.lock().await;
            remove_pending_callback(
                &conn,
                &wp_base,
                relation_id_i64,
                object_type_key,
                object_id_i64,
            )
            .unwrap_or(false)
        } else {
            let store = pending_store.lock().await;
            store
                .remove(&wp_base, relation_id, object_type_key, object_id)
                .unwrap_or(false)
        };
        if cleared {
            let _ = log_event(
                &log_file,
                "info",
                "discovery.resync_pending_callback_cleared",
                json!({
                    "relation_id": relation_id,
                    "object_id": object_id,
                    "object_type": object_type_key
                }),
            );
        }
    }

    let existing: Option<(
        String,
        String,
        TranslationCallbackPayload,
        Option<String>,
        u32,
    )> = if item.needs_resync {
        None
    } else if let Some(ref db_arc) = db {
        let conn = db_arc.lock().await;
        find_pending_callback(
            &conn,
            &wp_base,
            relation_id_i64,
            object_type_key,
            object_id_i64,
        )
        .map(|entry| {
            (
                entry.idempotency_key,
                entry.payload.worker_id.clone(),
                entry.payload,
                entry.route_secret,
                entry.retry_count,
            )
        })
    } else {
        let store = pending_store.lock().await;
        store
            .find(&wp_base, relation_id, object_type_key, object_id)
            .map(|entry| {
                (
                    entry.idempotency_key.clone(),
                    entry.payload.worker_id.clone(),
                    entry.payload.clone(),
                    entry.route_secret.clone(),
                    entry.retry_count,
                )
            })
    };

    if let Some((idem_key, worker_id_val, payload_val, route_secret_val, retry_count_val)) =
        existing
    {
        let effective_route_secret = route_secret.as_deref().or(route_secret_val.as_deref());
        let _cb_permit = callback_sem.acquire().await.ok();
        let _global_cb_permit = if let Some(sem) = global_callback_sem.as_ref() {
            sem.clone().acquire_owned().await.ok()
        } else {
            None
        };
        adaptive.wait_turn().await;
        match send_translation_callback(
            &client,
            &wp_base,
            &token,
            &worker_id_val,
            &idem_key,
            &payload_val,
            effective_route_secret,
        )
        .await
        {
            Ok(_) => {
                adaptive.on_success();
                if let Some(ref db_arc) = db {
                    let conn = db_arc.lock().await;
                    let _ = remove_pending_callback(
                        &conn,
                        &wp_base,
                        relation_id_i64,
                        object_type_key,
                        object_id_i64,
                    );
                } else {
                    let store = pending_store.lock().await;
                    let _ = store.remove(&wp_base, relation_id, object_type_key, object_id);
                }
                return Ok(true);
            }
            Err(err) => {
                adaptive.on_error_message(&format!("{:#}", err));
                if let Some(ref db_arc) = db {
                    let conn = db_arc.lock().await;
                    let _ = increment_retry_pending_callback(
                        &conn,
                        &wp_base,
                        relation_id_i64,
                        object_type_key,
                        object_id_i64,
                    );
                } else {
                    let store = pending_store.lock().await;
                    let _ =
                        store.increment_retry(&wp_base, relation_id, object_type_key, object_id);
                }
                let _ = log_event(
                    &log_file,
                    "warning",
                    "discovery.callback_retry_failed",
                    json!({
                        "relation_id": relation_id,
                        "object_id": object_id,
                        "retry_count": retry_count_val + 1,
                        "error": snippet(&format!("{:#}", err))
                    }),
                );
                return Ok(false);
            }
        }
    }

    if !item.needs_resync {
        if let Some(ref db_arc) = db {
            let already_done = {
                let conn = db_arc.lock().await;
                crate::db::translations::has_materialized_success_record(
                    &conn,
                    &wp_base,
                    relation_id_i64,
                    object_id_i64,
                    object_type_key,
                )
            };
            if already_done {
                let has_pending = {
                    let conn = db_arc.lock().await;
                    find_pending_callback(
                        &conn,
                        &wp_base,
                        relation_id_i64,
                        object_type_key,
                        object_id_i64,
                    )
                };
                if let Some(pending) = has_pending {
                    let effective_route_secret =
                        route_secret.as_deref().or(pending.route_secret.as_deref());
                    let _cb_permit = callback_sem.acquire().await.ok();
                    let _global_cb_permit = if let Some(sem) = global_callback_sem.as_ref() {
                        sem.clone().acquire_owned().await.ok()
                    } else {
                        None
                    };
                    let mut callback_ok = false;
                    adaptive.wait_turn().await;
                    match send_translation_callback(
                        &client,
                        &wp_base,
                        &token,
                        &pending.payload.worker_id,
                        &pending.idempotency_key,
                        &pending.payload,
                        effective_route_secret,
                    )
                    .await
                    {
                        Ok(_) => {
                            adaptive.on_success();
                            callback_ok = true;
                            let conn = db_arc.lock().await;
                            let _ = remove_pending_callback(
                                &conn,
                                &wp_base,
                                relation_id_i64,
                                object_type_key,
                                object_id_i64,
                            );
                        }
                        Err(err) => {
                            adaptive.on_error_message(&format!("{:#}", err));
                            let _ = log_event(
                                &log_file,
                                "warning",
                                "discovery.dedup_callback_retry_failed",
                                json!({
                                    "relation_id": relation_id,
                                    "object_id": object_id,
                                    "error": snippet(&format!("{:#}", err))
                                }),
                            );
                        }
                    }
                    if callback_ok {
                        return Ok(true);
                    }
                    return Ok(false);
                } else {
                    let _ = log_event(
                        &log_file,
                        "debug",
                        "discovery.item_already_translated",
                        json!({
                            "relation_id": relation_id,
                            "object_id": object_id
                        }),
                    );
                    return Ok(false);
                }
            }
        }
    } else {
        let _ = log_event(
            &log_file,
            "debug",
            "discovery.resync_forced",
            json!({
                "relation_id": relation_id,
                "object_id": object_id,
                "object_type": object_type_key
            }),
        );
    }

    let start = std::time::Instant::now();
    let raw_result = translate_content_item(
        &client,
        &wp_base,
        &token,
        &log_file,
        &item,
        &relation,
        &rules,
        component_registry.as_deref(),
        proxy_pool.as_deref(),
        &component_id_override,
        &component_prefer_ids,
        task_type_component_bindings.as_deref(),
        rule_component_bindings.as_deref(),
        &worker_config,
        route_secret.as_deref(),
        &pending_store,
        db.clone(),
        &callback_sem,
        global_translation_sem.clone(),
        global_callback_sem.clone(),
        Arc::clone(&adaptive),
        &task_params,
    )
    .await;
    let exec_ms = start.elapsed().as_millis() as i64;
    let mut item_fields_count: i32 = 0;
    let mut item_status = "failed".to_string();
    let mut record_status = "failed".to_string();
    let mut done_delta = 0i64;
    let mut fail_delta = 0i64;
    let mut error_msg: Option<String> = None;
    let mut execution_summary = TranslationExecutionSummary::default();
    let result: anyhow::Result<bool> = match raw_result {
        Ok((outcome, fc, summary)) => {
            item_fields_count = fc as i32;
            execution_summary = summary;
            match outcome {
                TranslateContentOutcome::Completed => {
                    item_status = "done".to_string();
                    record_status = "success".to_string();
                    done_delta = 1;
                    Ok(true)
                }
                TranslateContentOutcome::NoChanges => {
                    item_status = "done".to_string();
                    record_status = "skipped".to_string();
                    done_delta = 1;
                    Ok(false)
                }
                TranslateContentOutcome::PendingReview => {
                    item_status = "pending_review".to_string();
                    record_status = "pending_callback".to_string();
                    Ok(false)
                }
                TranslateContentOutcome::PendingCallback => {
                    item_status = "translated".to_string();
                    record_status = "pending_callback".to_string();
                    Ok(false)
                }
            }
        }
        Err(err) => {
            error_msg = Some(format!("{:#}", err));
            fail_delta = 1;
            Err(err)
        }
    };

    if let Some(ref db_arc) = db {
        let object_type_key = normalize_object_type_key(&item.object_type).to_string();
        let matched_rule = rules
            .iter()
            .find(|r| r.object_name == item.subtype)
            .or_else(|| rules.iter().find(|r| r.object_name == item.object_type));
        let derived_business_line = crate::task_engine::pipeline::derive_business_line_from_rule(
            matched_rule,
            &item.object_type,
        )
        .to_string();
        {
            let conn = db_arc.lock().await;
            let _ = crate::db::translations::insert_translation_record(
                &conn,
                &crate::db::translations::InsertTranslationRecord {
                    domain: wp_base.clone(),
                    relation_id: Some(relation.id),
                    object_id: Some(item.object_id),
                    object_type: Some(object_type_key.clone()),
                    business_line: Some(derived_business_line.clone()),
                    source_lang: relation.source_lang.clone(),
                    target_lang: relation.target_lang.clone(),
                    status: record_status.clone(),
                    execution_ms: Some(exec_ms),
                    worker_id: Some(worker_config.worker_id.clone()),
                    idempotency_key: None,
                    fields_count: item_fields_count,
                    error_message: error_msg.clone(),
                    component_ids: execution_summary.component_ids.clone(),
                    media_mappings_count: execution_summary.media_mappings_count,
                    failed_fields_count: execution_summary.failed_fields_count,
                    primary_failure_reason: execution_summary.primary_failure_reason.clone(),
                },
            );
            if let Some(jid) = job_id {
                let data_dir = crate::config::env_or("WPTSALL_DATA_DIR", DEFAULT_DATA_DIR);
                let domain_key_for_paths = pipeline_sanitize_domain_key(&wp_base);
                let raw_path = format!(
                    "{}/raw/{}/rel_{}/{}_{}.json",
                    data_dir, domain_key_for_paths, relation.id, item.object_type, item.object_id
                );
                let translated_path =
                    build_translated_path(&raw_path, &data_dir, &domain_key_for_paths);
                let persisted_component_ids = execution_summary.component_ids.clone();
                let persisted_component_id =
                    persisted_component_ids.first().cloned().unwrap_or_default();
                let _ = crate::db::jobs::create_item(
                    &conn,
                    &crate::db::jobs::CreateItemRequest {
                        job_id: jid,
                        domain: wp_base.clone(),
                        relation_id: relation.id,
                        business_line: derived_business_line.clone(),
                        object_type: object_type_key.clone(),
                        wp_object_id: item.object_id,
                        wp_object_subtype: item.subtype.clone(),
                        task_type: "text".to_string(),
                        source_lang: relation.source_lang.clone(),
                        target_lang: relation.target_lang.clone(),
                        component_id: persisted_component_id.clone(),
                        component_ids: persisted_component_ids.clone(),
                        selected_component_id: task_params.selected_component_id.clone(),
                        effective_source_lang: task_params
                            .effective_source_lang
                            .clone()
                            .or(Some(relation.source_lang.clone())),
                        effective_target_lang: task_params
                            .effective_target_lang
                            .clone()
                            .or(Some(relation.target_lang.clone())),
                        editable_overrides: task_params.editable_overrides.clone(),
                        raw_path: raw_path.clone(),
                        client_task_id: String::new(),
                        max_retries: 2,
                    },
                )
                .and_then(|item_db_id| {
                    let _ = crate::db::jobs::update_item_component_trace(
                        &conn,
                        item_db_id,
                        &persisted_component_id,
                        &persisted_component_ids,
                    );
                    let _ = crate::db::jobs::update_item_translated_path(
                        &conn,
                        item_db_id,
                        &translated_path,
                    );
                    crate::db::jobs::update_item_status(
                        &conn,
                        item_db_id,
                        item_status.as_str(),
                        error_msg.as_deref(),
                    )
                });
                let _ =
                    crate::db::jobs::increment_job_counters(&conn, jid, done_delta, fail_delta, 1);
            }
        }
    }

    result
}

/// Translate a single content item and submit the result to WP.
///
/// Uses the persistent pipeline: translate -> persist to disk -> sync to WP.
/// Each step updates the item's status in DB, enabling crash recovery.
#[allow(clippy::too_many_arguments)]
pub(super) async fn translate_content_item(
    client: &Client,
    wp_base: &str,
    token: &str,
    log_file: &str,
    item: &ContentItem,
    relation: &DiscoveredRelation,
    rules: &[DiscoveredRule],
    component_registry: Option<&ComponentRuntimeRegistry>,
    proxy_pool: Option<&ProxyClientPool>,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<&TaskTypeComponentBindingsDoc>,
    rule_component_bindings: Option<&RuleComponentBindingsDoc>,
    worker_config: &WorkerConfig,
    route_secret: Option<&str>,
    pending_store: &Arc<Mutex<PendingCallbackStore>>,
    db: Option<Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
    callback_sem: &Arc<Semaphore>,
    global_translation_sem: Option<Arc<Semaphore>>,
    global_callback_sem: Option<Arc<Semaphore>>,
    adaptive: Arc<AdaptiveRateControl>,
    task_params: &DiscoveryTaskParams,
) -> anyhow::Result<(TranslateContentOutcome, usize, TranslationExecutionSummary)> {
    let relation_id_i64 = relation.id;
    let relation_id_u64 = u64::try_from(relation.id).unwrap_or(0);
    let object_id_i64 = item.object_id;
    let effective_component_id_override = task_params
        .selected_component_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(component_id_override);
    let scoped_registry = build_task_scoped_runtime_registry(
        component_registry,
        task_params.selected_component_id.as_deref(),
        task_params.editable_overrides.as_ref(),
    )?;
    let effective_registry = scoped_registry.as_ref().or(component_registry);
    let mut effective_relation = relation.clone();
    if let Some(ref source_lang) = task_params.effective_source_lang {
        effective_relation.source_lang = source_lang.clone();
    }
    if let Some(ref target_lang) = task_params.effective_target_lang {
        effective_relation.target_lang = target_lang.clone();
    }

    let _global_translate_permit = if let Some(sem) = global_translation_sem.as_ref() {
        sem.clone().acquire_owned().await.ok()
    } else {
        None
    };
    adaptive.wait_turn().await;
    let result = match translate_item_fields_with_trace_using_proxy(
        client,
        proxy_pool,
        wp_base,
        item,
        &effective_relation,
        rules,
        effective_registry,
        effective_component_id_override,
        component_prefer_ids,
        task_type_component_bindings,
        rule_component_bindings,
        worker_config,
        log_file,
    )
    .await
    {
        Ok(v) => {
            adaptive.on_success();
            v
        }
        Err(err) => {
            adaptive.on_error_message(&format!("{:#}", err));
            return Err(err);
        }
    };

    let trace = match result {
        Some(trace) => trace,
        None => {
            return Ok((
                TranslateContentOutcome::NoChanges,
                0,
                TranslationExecutionSummary::default(),
            ))
        }
    };
    let mut payload = trace.payload;
    let idempotency_key = trace.idempotency_key;
    let actual_component_ids = trace.component_ids.clone();
    if actual_component_ids.len() > 1 {
        let _ = log_event(
            log_file,
            "warning",
            "discovery.multi_component_item",
            json!({
                "relation_id": relation.id,
                "object_id": item.object_id,
                "component_ids": actual_component_ids,
            }),
        );
    }

    let (policy, dsl) = if let Some(ref db_arc) = db {
        let conn = db_arc.lock().await;
        (
            crate::task_engine::workflow_policy::load_workflow_policy(
                &conn,
                worker_config.review_mode,
            ),
            crate::task_engine::workflow_dsl::load_workflow_dsl(&conn),
        )
    } else {
        (
            crate::task_engine::workflow_policy::WorkflowPolicy::from_review_mode_flag(
                worker_config.review_mode,
            ),
            crate::task_engine::workflow_dsl::WorkflowDsl::default(),
        )
    };

    // Media step is DSL-gated: default pipeline includes it; custom DSL can omit.
    if !payload.media_mappings.is_empty()
        && crate::task_engine::workflow_interpreter::should_run_media_step(&dsl)
    {
        if let Err(err) = upload_pending_media(
            client,
            wp_base,
            token,
            &worker_config.worker_id,
            relation_id_i64,
            relation_id_u64,
            &mut payload,
            log_file,
            route_secret,
        )
        .await
        {
            let _ = log_event(
                log_file,
                "warning",
                "discovery.media_upload_prepare_failed",
                json!({
                    "relation_id": relation.id,
                    "object_id": item.object_id,
                    "error": snippet(&format!("{:#}", err))
                }),
            );
        }
    } else if !payload.media_mappings.is_empty() {
        let _ = log_event(
            log_file,
            "info",
            "discovery.media_step_skipped",
            json!({
                "relation_id": relation.id,
                "object_id": item.object_id,
                "media_mappings": payload.media_mappings.len(),
            }),
        );
    }

    let fields_count = payload.translated_fields.len();
    let execution_summary =
        build_translation_execution_summary(&payload, actual_component_ids.clone());
    let data_dir = crate::config::env_or("WPTSALL_DATA_DIR", DEFAULT_DATA_DIR);
    if let Some(ref db_arc) = db {
        let domain_key = pipeline_sanitize_domain_key(wp_base);
        let raw_path = format!(
            "{}/raw/{}/rel_{}/{}_{}.json",
            data_dir, domain_key, relation.id, item.object_type, item.object_id
        );
        let translated_path = build_translated_path(&raw_path, &data_dir, &domain_key);
        let _ = persist_raw_content(db_arc, 0, &item.complete_data, &raw_path, log_file).await;
        let _ = persist_translated(
            db_arc,
            0,
            &payload,
            &idempotency_key,
            route_secret,
            &translated_path,
            log_file,
        )
        .await;
    }

    {
        let sample_format = payload.field_results.iter().find_map(|f| {
            let fmt = f.content_format.trim();
            if fmt.is_empty() {
                None
            } else {
                Some(fmt)
            }
        });
        let domain_host = wp_base
            .trim()
            .trim_end_matches('/')
            .split("://")
            .nth(1)
            .unwrap_or(wp_base);
        if crate::task_engine::workflow_interpreter::resolve_post_translate_action(
            &dsl,
            &policy,
            Some(domain_host),
            sample_format,
            None,
        ) == crate::task_engine::workflow_dsl::PostTranslateAction::PendingReview
        {
            if let Some(ref db_arc) = db {
                let conn = db_arc.lock().await;
                let items = crate::db::jobs::list_items_by_domain_object(
                    &conn,
                    wp_base,
                    relation_id_i64,
                    object_id_i64,
                );
                for it in &items {
                    let _ =
                        crate::db::jobs::update_item_status(&conn, it.id, "pending_review", None);
                }
            }
            let _ = log_event(
                log_file,
                "info",
                "discovery.item_pending_review",
                json!({
                    "relation_id": relation.id,
                    "object_id": item.object_id,
                    "fields_count": fields_count,
                    "workflow_mode": "review"
                }),
            );
            return Ok((
                TranslateContentOutcome::PendingReview,
                fields_count,
                execution_summary.clone(),
            ));
        }
    }

    let route_secret_owned = route_secret.map(|s| s.to_string());
    if let Some(ref db_arc) = db {
        let db_entry = PendingCallbackEntry {
            api_base_url: wp_base.to_string(),
            idempotency_key: idempotency_key.clone(),
            payload: payload.clone(),
            route_secret: route_secret_owned.clone(),
            created_at: unix_ts(),
            retry_count: 0,
            last_retry_at: 0,
            relation_id: relation_id_i64,
            object_id: object_id_i64,
            object_type: normalize_object_type_key(&payload.object_type).to_string(),
        };
        {
            let conn = db_arc.lock().await;
            let _ = add_pending_callback(&conn, &db_entry);
        }
    } else {
        let entry = PendingCallback {
            api_base_url: wp_base.to_string(),
            idempotency_key: idempotency_key.clone(),
            payload: payload.clone(),
            route_secret: route_secret_owned.clone(),
            created_at: unix_ts(),
            retry_count: 0,
            last_retry_at: 0,
        };
        let store = pending_store.lock().await;
        store.add(entry)?;
    }

    let _cb_permit = callback_sem.acquire().await.ok();
    let _global_cb_permit = if let Some(sem) = global_callback_sem.as_ref() {
        sem.clone().acquire_owned().await.ok()
    } else {
        None
    };
    adaptive.wait_turn().await;
    let cb_result = {
        let client_c = client.clone();
        let wp_base_c = wp_base.to_string();
        let token_c = token.to_string();
        let worker_id_c = worker_config.worker_id.clone();
        let idempotency_key_c = idempotency_key.clone();
        let payload_c = payload.clone();
        let route_secret_c = route_secret_owned.clone();
        retry_with_backoff(
            "discovery.item_callback",
            relation_id_i64,
            log_file,
            worker_config,
            |_attempt| {
                let cl = client_c.clone();
                let wb = wp_base_c.clone();
                let tk = token_c.clone();
                let wi = worker_id_c.clone();
                let ik = idempotency_key_c.clone();
                let pl = payload_c.clone();
                let rs = route_secret_c.clone();
                async move {
                    send_translation_callback(&cl, &wb, &tk, &wi, &ik, &pl, rs.as_deref()).await
                }
            },
        )
        .await
    };
    match cb_result {
        Ok(_) => {
            adaptive.on_success();
            if let Some(ref db_arc) = db {
                let conn = db_arc.lock().await;
                let _ = remove_pending_callback(
                    &conn,
                    wp_base,
                    relation_id_i64,
                    normalize_object_type_key(&payload.object_type),
                    object_id_i64,
                );
            } else {
                let store = pending_store.lock().await;
                let _ = store.remove(
                    wp_base,
                    payload.relation_id,
                    normalize_object_type_key(&payload.object_type),
                    payload.object_id,
                );
            }
            let _ = log_event(
                log_file,
                "info",
                "discovery.callback_sent",
                json!({
                    "relation_id": payload.relation_id,
                    "object_id": payload.object_id,
                }),
            );
        }
        Err(err) => {
            adaptive.on_error_message(&format!("{:#}", err));
            let _ = log_event(
                log_file,
                "warning",
                "discovery.callback_failed",
                json!({
                    "relation_id": payload.relation_id,
                    "object_id": payload.object_id,
                    "error": format!("{:#}", err)
                }),
            );
            return Ok((
                TranslateContentOutcome::PendingCallback,
                fields_count,
                execution_summary.clone(),
            ));
        }
    }

    let _ = log_event(
        log_file,
        "info",
        "discovery.item_translated",
        json!({
            "relation_id": relation.id,
            "object_id": item.object_id,
            "object_type": item.object_type,
            "fields_count": fields_count
        }),
    );

    Ok((
        TranslateContentOutcome::Completed,
        fields_count,
        execution_summary,
    ))
}
