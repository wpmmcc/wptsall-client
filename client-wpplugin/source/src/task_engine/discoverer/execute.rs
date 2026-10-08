use anyhow::Context;
use std::sync::Arc;

use serde_json::json;
use tokio::sync::{Mutex, Semaphore};

use super::*;
use crate::task_engine::submitter::upload_pending_media_with_optional_db;

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
    pub client_task_id: String,
    pub media_mappings_count: i32,
    pub failed_fields_count: i32,
    pub primary_failure_reason: Option<String>,
    completion: Option<CallbackCompletion>,
}

#[derive(Debug, Clone)]
struct CallbackCompletion {
    identity: String,
    payload: TranslationCallbackPayload,
    ack: serde_json::Value,
    authority: CallbackAuthority,
}

#[derive(Debug, Clone)]
struct CallbackAuthority {
    route_secret: Option<String>,
    pending: (String, bool),
    files: std::collections::BTreeMap<String, String>,
}

fn callback_file_digest(path: &str) -> anyhow::Result<String> {
    use sha2::Digest;
    Ok(format!("{:x}", sha2::Sha256::digest(std::fs::read(path)?)))
}

async fn capture_callback_authority(
    db: &Arc<Mutex<rusqlite::Connection>>,
    lease: &Option<Arc<crate::db::unit_lock::ContentExecution>>,
    base: &str,
    identity: &str,
    payload: &TranslationCallbackPayload,
    item: &ContentItem,
) -> anyhow::Result<CallbackAuthority> {
    let conn = db.lock().await;
    lease
        .as_ref()
        .context("content execution authority missing")?
        .assert_owner(&conn, db)?;
    let saved = find_pending_callback(
        &conn,
        base,
        i64::try_from(payload.relation_id)?,
        &payload.object_type,
        i64::try_from(payload.object_id)?,
    )?
    .context("original pending callback missing; retained")?;
    let pending = crate::db::pending_callbacks::continuation_authority(
        &conn,
        base,
        identity,
        payload,
        saved.route_secret.as_deref(),
    )?;
    let mut files = std::collections::BTreeMap::new();
    for saved in crate::db::jobs::list_items_by_domain_object_checked(
        &conn,
        base,
        i64::try_from(payload.relation_id)?,
        i64::try_from(payload.object_id)?,
    )? {
        if saved.client_task_id == payload.client_task_id
            && crate::db::pending_callbacks::normalize_pending_object_type(&saved.object_type)?
                == crate::db::pending_callbacks::normalize_pending_object_type(
                    &payload.object_type,
                )?
            && !saved.translated_path.is_empty()
        {
            files.insert(
                saved.translated_path.clone(),
                callback_file_digest(&saved.translated_path)?,
            );
        }
    }
    let (_, path) = item_disk_paths(base, i64::try_from(payload.relation_id)?, item);
    if std::path::Path::new(&path).exists() {
        files.insert(path.clone(), callback_file_digest(&path)?);
    }
    Ok(CallbackAuthority {
        route_secret: saved.route_secret,
        pending,
        files,
    })
}

fn check_callback_authority(
    conn: &rusqlite::Connection,
    base: &str,
    identity: &str,
    payload: &TranslationCallbackPayload,
    authority: &CallbackAuthority,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        crate::db::pending_callbacks::continuation_authority(
            conn,
            base,
            identity,
            payload,
            authority.route_secret.as_deref(),
        )? == authority.pending,
        "original pending callback changed; retained"
    );
    for (path, digest) in &authority.files {
        anyhow::ensure!(
            callback_file_digest(path)? == *digest,
            "original callback result changed; retained"
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn mutate_saved_callback<R>(
    db: &Arc<Mutex<rusqlite::Connection>>,
    lease: &Option<Arc<crate::db::unit_lock::ContentExecution>>,
    base: &str,
    identity: &str,
    payload: &TranslationCallbackPayload,
    authority: &CallbackAuthority,
    change: impl FnOnce(&rusqlite::Connection) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    let mut conn = db.lock().await;
    let lease = lease
        .as_ref()
        .context("content execution authority missing")?;
    lease.assert_owner(&conn, db)?;
    check_callback_authority(&conn, base, identity, payload, authority)?;
    let credit = match conn
        .path()
        .filter(|path| !path.is_empty() && authority.pending.1)
    {
        Some(path) => {
            crate::storage_capacity::database_recovery_credit(std::path::Path::new(path), false)?
        }
        None => None,
    };
    crate::storage_capacity::with_database_credit(credit, || -> anyhow::Result<R> {
        let tx = conn.savepoint()?;
        lease.assert_owner(&tx, db)?;
        check_callback_authority(&tx, base, identity, payload, authority)?;
        let result = change(&tx)?;
        lease.assert_owner(&tx, db)?;
        tx.commit()?;
        Ok(result)
    })
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
        client_task_id: payload.client_task_id.clone(),
        media_mappings_count: i32::try_from(payload.media_mappings.len()).unwrap_or(i32::MAX),
        failed_fields_count: i32::try_from(failed_fields.len()).unwrap_or(i32::MAX),
        primary_failure_reason,
        completion: None,
    }
}

/// The on-disk raw/translated path pair for one content item.
fn item_disk_paths(wp_base: &str, relation_id: i64, item: &ContentItem) -> (String, String) {
    let data_dir = crate::config::env_or("WPTSALL_DATA_DIR", DEFAULT_DATA_DIR);
    let domain_key = pipeline_sanitize_domain_key(wp_base);
    let snapshot = crate::db::system::private_json_digest(&item.complete_data)
        .expect("JSON content snapshot can be hashed");
    let raw_path = format!(
        "{}/raw/{}/rel_{}/{}_{}_{}.json",
        data_dir, domain_key, relation_id, item.object_type, item.object_id, snapshot
    );
    let translated_path = build_translated_path(&raw_path, &data_dir, &domain_key);
    (raw_path, translated_path)
}

/// Create the job-item row for a translated content item so the jobs
/// dashboard and the pending-review pool can see and act on it.
///
/// Shared by the relation-scan path (`translate_content_item_owned`, which
/// owns its own row creation) and the outbox/retry fast paths, which thread
/// a job id into `translate_content_item`. Without this, outbox-discovered
/// items held for review were invisible to every review surface and could
/// never be approved (found by the SIM-04 simulation journey).
#[allow(clippy::too_many_arguments)]
fn create_job_item_row(
    conn: &rusqlite::Connection,
    job_id: i64,
    wp_base: &str,
    relation: &DiscoveredRelation,
    item: &ContentItem,
    rules: &[DiscoveredRule],
    task_params: &DiscoveryTaskParams,
    execution_summary: &TranslationExecutionSummary,
    raw_path: &str,
    translated_path: &str,
    item_status: &str,
    error_msg: Option<&str>,
) -> anyhow::Result<()> {
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
    let persisted_component_ids = execution_summary.component_ids.clone();
    let persisted_component_id = persisted_component_ids.first().cloned().unwrap_or_default();
    crate::db::jobs::record_item_outcome(
        conn,
        &crate::db::jobs::CreateItemRequest {
            job_id,
            domain: wp_base.to_string(),
            relation_id: relation.id,
            business_line: derived_business_line,
            object_type: object_type_key,
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
            raw_path: raw_path.to_string(),
            client_task_id: execution_summary.client_task_id.clone(),
            max_retries: 2,
        },
        translated_path,
        item_status,
        error_msg,
    )?;
    Ok(())
}

async fn mutate_content<R>(
    db: &Arc<Mutex<rusqlite::Connection>>,
    lease: &Option<Arc<crate::db::unit_lock::ContentExecution>>,
    change: impl FnOnce(&rusqlite::Connection) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    let lease = lease
        .as_ref()
        .context("content execution authority missing")?;
    crate::storage_capacity::with_result_credit(None, lease.mutate(db, change)).await
}

async fn restore_completed_callback_for_resync(
    db: &Arc<Mutex<rusqlite::Connection>>,
    lease: &Option<Arc<crate::db::unit_lock::ContentExecution>>,
    base: &str,
    relation: i64,
    item: &ContentItem,
    route_secret: Option<String>,
) -> anyhow::Result<()> {
    mutate_content(db, lease, |conn| {
        let Some(saved) = super::receipts::completed(conn, base, relation, item)? else {
            return Ok(());
        };
        add_pending_callback(
            conn,
            &PendingCallbackEntry {
                api_base_url: base.to_string(),
                idempotency_key: saved.identity,
                payload: saved.payload,
                route_secret,
                created_at: unix_ts(),
                retry_count: 0,
                last_retry_at: 0,
                relation_id: relation,
                object_id: item.object_id,
                object_type: item.object_type.clone(),
            },
        )
    })
    .await
}

fn confirm_callback_in(
    conn: &rusqlite::Connection,
    base: &str,
    identity: &str,
    payload: &TranslationCallbackPayload,
    item: &ContentItem,
    ack: &serde_json::Value,
) -> anyhow::Result<()> {
    let relation = i64::try_from(payload.relation_id)?;
    let object = i64::try_from(payload.object_id)?;
    let object_type =
        crate::db::pending_callbacks::normalize_pending_object_type(&payload.object_type)?;
    let pending = find_pending_callback(conn, base, relation, object_type, object)?
        .context("original pending callback missing; receipt retained")?;
    anyhow::ensure!(
        pending.idempotency_key == identity
            && crate::db::system::private_json_digest(&serde_json::to_value(&pending.payload)?)?
                == crate::db::system::private_json_digest(&serde_json::to_value(payload)?)?,
        "pending callback changed; original receipt retained"
    );
    super::receipts::save(conn, base, identity, payload, item, ack)?;
    let items = crate::db::jobs::list_items_by_domain_object_checked(conn, base, relation, object)?;
    let matches: Vec<_> = items
        .into_iter()
        .filter(|item| {
            crate::db::pending_callbacks::normalize_pending_object_type(&item.object_type).ok()
                == Some(object_type)
                && item.client_task_id == payload.client_task_id
        })
        .collect();
    for item in &matches {
        crate::db::jobs::complete_saved_callback(
            conn,
            item.id,
            base,
            identity,
            &serde_json::to_string(ack)?,
        )?;
    }
    if matches.is_empty() {
        let key = format!(
            "discovery-callback-receipt-v1:{}",
            crate::db::system::private_json_digest(&json!({
                "site":base,"identity":identity,"payload":payload,
            }))?
        );
        let saved = crate::db::system::encrypt_config_value(&serde_json::to_string(&json!({
            "format":"discovery-callback-receipt-v1","identity":identity,"payload":payload,"ack":ack,
        }))?)?;
        if let Some(original) = crate::db::system::get_system_config_checked(conn, &key)? {
            anyhow::ensure!(
                original.starts_with("V1BUQw"),
                "callback receipt is not encrypted; retained"
            );
            let original: serde_json::Value =
                serde_json::from_str(&crate::db::system::decrypt_config_value(&original)?)
                    .context("damaged callback receipt; retained")?;
            anyhow::ensure!(
                original["format"] == "discovery-callback-receipt-v1"
                    && original["identity"] == identity
                    && crate::db::system::private_json_digest(&original["payload"])?
                        == crate::db::system::private_json_digest(&serde_json::to_value(payload)?)?,
                "callback receipt identity differs; original retained"
            );
            crate::task_engine::submitter::validate_translation_callback_ack(
                original["ack"].clone(),
            )?;
        } else {
            let changed = conn.execute(
                "INSERT INTO system_config(key,value) VALUES (?1,?2)",
                rusqlite::params![key, saved],
            )?;
            anyhow::ensure!(
                changed == 1
                    && crate::db::system::get_system_config_checked(conn, &key)?.as_deref()
                        == Some(saved.as_str()),
                "callback receipt was not committed; original pending result retained"
            );
        }
        let record = crate::db::translations::insert_translation_record(
            conn,
            &crate::db::translations::InsertTranslationRecord {
                domain: base.into(),
                relation_id: Some(relation),
                object_id: Some(object),
                object_type: Some(object_type.into()),
                business_line: Some(payload.business_line.clone()),
                source_lang: payload.source_lang.clone(),
                target_lang: payload.target_lang.clone(),
                status: "pending".into(),
                idempotency_key: Some(identity.into()),
                fields_count: i32::try_from(payload.translated_fields.len())?,
                media_mappings_count: i32::try_from(payload.media_mappings.len())?,
                ..Default::default()
            },
        )?;
        crate::db::translations::mark_callback_sent(conn, record)?;
        anyhow::ensure!(
            remove_pending_callback(conn, base, relation, object_type, object)?,
            "original callback was not removed; retained"
        );
    }
    Ok(())
}

async fn confirm_callback(
    db: &Arc<Mutex<rusqlite::Connection>>,
    lease: &Option<Arc<crate::db::unit_lock::ContentExecution>>,
    base: &str,
    identity: &str,
    payload: &TranslationCallbackPayload,
    item: &ContentItem,
    ack: &serde_json::Value,
    authority: &CallbackAuthority,
) -> anyhow::Result<()> {
    mutate_saved_callback(db, lease, base, identity, payload, authority, |conn| {
        confirm_callback_in(conn, base, identity, payload, item, ack)
    })
    .await
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
    let db = match db {
        Some(db) => Some(db),
        None => Some(pending_store.lock().await.database()),
    };
    let content_lease = match db.as_ref() {
        Some(db) => Some(
            crate::db::unit_lock::ContentExecution::acquire(
                db,
                &wp_base,
                relation.id,
                &item.object_type,
                item.object_id,
            )
            .await?,
        ),
        None => None,
    };
    let object_id = u64::try_from(item.object_id).unwrap_or(0);
    let relation_id = u64::try_from(relation.id).unwrap_or(0);
    let object_id_i64 = item.object_id;
    let relation_id_i64 = relation.id;
    let object_type_key = normalize_object_type_key(&item.object_type);

    if item.needs_resync {
        let unresolved = if let Some(ref db_arc) = db {
            let conn = db_arc.lock().await;
            find_pending_callback(
                &conn,
                &wp_base,
                relation_id_i64,
                object_type_key,
                object_id_i64,
            )?
            .is_some()
        } else {
            let store = pending_store.lock().await;
            store
                .find(&wp_base, relation_id, object_type_key, object_id)?
                .is_some()
        };
        anyhow::ensure!(
            !unresolved,
            "source changed while original callback is unconfirmed; retained"
        );
    }

    if item.needs_resync {
        if let Some(db) = db.as_ref() {
            restore_completed_callback_for_resync(
                db,
                &content_lease,
                &wp_base,
                relation.id,
                &item,
                route_secret.clone(),
            )
            .await?;
        }
    }
    let existing: Option<(String, String, TranslationCallbackPayload, u32)> =
        if let Some(ref db_arc) = db {
            let conn = db_arc.lock().await;
            find_pending_callback(
                &conn,
                &wp_base,
                relation_id_i64,
                object_type_key,
                object_id_i64,
            )?
            .map(|entry| {
                (
                    entry.idempotency_key,
                    entry.payload.worker_id.clone(),
                    entry.payload,
                    entry.retry_count,
                )
            })
        } else {
            let store = pending_store.lock().await;
            store
                .find(&wp_base, relation_id, object_type_key, object_id)?
                .map(|entry| {
                    (
                        entry.idempotency_key.clone(),
                        entry.payload.worker_id.clone(),
                        entry.payload.clone(),
                        entry.retry_count,
                    )
                })
        };

    if let Some((idem_key, worker_id_val, payload_val, retry_count_val)) = existing {
        let authority = match db.as_ref() {
            Some(db) => Some(
                capture_callback_authority(
                    db,
                    &content_lease,
                    &wp_base,
                    &idem_key,
                    &payload_val,
                    &item,
                )
                .await?,
            ),
            None => None,
        };
        let _cb_permit = callback_sem
            .acquire()
            .await
            .context("callback budget is closed; retained")?;
        let _global_cb_permit = if let Some(sem) = global_callback_sem.as_ref() {
            Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .context("global callback budget is closed; retained")?,
            )
        } else {
            None
        };
        adaptive.wait_turn().await;
        match send_translation_callback(
            &client,
            &wp_base,
            &token,
            &worker_id_val,
            &worker_config.device_id,
            &idem_key,
            &payload_val,
        )
        .await
        {
            Ok(ack) => {
                adaptive.on_success();
                if let Some(ref db_arc) = db {
                    confirm_callback(
                        db_arc,
                        &content_lease,
                        &wp_base,
                        &idem_key,
                        &payload_val,
                        &item,
                        &ack,
                        authority
                            .as_ref()
                            .context("original callback authority missing")?,
                    )
                    .await?;
                } else {
                    let store = pending_store.lock().await;
                    anyhow::ensure!(
                        store.remove(&wp_base, relation_id, object_type_key, object_id)?,
                        "original callback was not removed; retained"
                    );
                }
                // FO-1 companion (SIM-15): the replay success used to be
                // audit-silent — the callback physically landed with no
                // callback_sent event, breaking the log-oracle chain for
                // parked-callback recovery journeys.
                let _ = log_event(
                    &log_file,
                    "info",
                    "discovery.callback_sent",
                    json!({
                        "relation_id": relation_id,
                        "object_id": object_id,
                        "via": "pending_replay",
                    }),
                );
                return Ok(true);
            }
            Err(err) => {
                adaptive.on_error_message(&format!("{:#}", err));
                if let Some(ref db_arc) = db {
                    mutate_content(db_arc, &content_lease, |conn| {
                        increment_retry_pending_callback(
                            conn,
                            &wp_base,
                            relation_id_i64,
                            object_type_key,
                            object_id_i64,
                        )
                    })
                    .await?;
                } else {
                    let store = pending_store.lock().await;
                    store.increment_retry(&wp_base, relation_id, object_type_key, object_id)?;
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
            if super::receipts::completed(&*db_arc.lock().await, &wp_base, relation.id, &item)?
                .is_some()
            {
                return Ok(false);
            }
            let already_done = {
                let conn = db_arc.lock().await;
                crate::db::translations::has_materialized_success_record_checked(
                    &conn,
                    &wp_base,
                    relation_id_i64,
                    object_id_i64,
                    object_type_key,
                )?
            };
            // FL-2a companion: an item already parked in the pending-review
            // pool is materialized work — the reviewer drives its terminal
            // state. Once a site actually serves the claim route (real
            // plugin contract), the scan lane otherwise re-translates
            // outbox-parked items and the pool double-lists them. The
            // parked item's OWN resync (needs_resync) still re-translates:
            // this branch is only reached when !needs_resync.
            let parked_in_review = {
                let conn = db_arc.lock().await;
                crate::db::jobs::has_pending_review_item_checked(
                    &conn,
                    &wp_base,
                    relation_id_i64,
                    object_id_i64,
                )?
            };
            if parked_in_review && !already_done {
                let _ = log_event(
                    &log_file,
                    "info",
                    "discovery.item_pending_review_dedup",
                    json!({
                        "relation_id": relation_id,
                        "object_id": object_id,
                        "object_type": item.object_type
                    }),
                );
                return Ok(false);
            }
            if already_done {
                let has_pending = {
                    let conn = db_arc.lock().await;
                    find_pending_callback(
                        &conn,
                        &wp_base,
                        relation_id_i64,
                        object_type_key,
                        object_id_i64,
                    )?
                };
                if let Some(pending) = has_pending {
                    let authority = capture_callback_authority(
                        db_arc,
                        &content_lease,
                        &wp_base,
                        &pending.idempotency_key,
                        &pending.payload,
                        &item,
                    )
                    .await?;
                    let _cb_permit = callback_sem
                        .acquire()
                        .await
                        .context("callback budget is closed; retained")?;
                    let _global_cb_permit = if let Some(sem) = global_callback_sem.as_ref() {
                        Some(
                            sem.clone()
                                .acquire_owned()
                                .await
                                .context("global callback budget is closed; retained")?,
                        )
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
                        &worker_config.device_id,
                        &pending.idempotency_key,
                        &pending.payload,
                    )
                    .await
                    {
                        Ok(ack) => {
                            adaptive.on_success();
                            callback_ok = true;
                            confirm_callback(
                                db_arc,
                                &content_lease,
                                &wp_base,
                                &pending.idempotency_key,
                                &pending.payload,
                                &item,
                                &ack,
                                &authority,
                            )
                            .await?;
                            // FO-1 companion (SIM-15): same audit gap as the
                            // pending_replay arm — the dedup re-delivery was
                            // invisible to the log oracle.
                            let _ = log_event(
                                &log_file,
                                "info",
                                "discovery.callback_sent",
                                json!({
                                    "relation_id": relation_id,
                                    "object_id": object_id,
                                    "via": "dedup_replay",
                                }),
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
        // The owned path creates the job-item row itself from the final
        // outcome; passing a job id here would duplicate the row.
        None,
        &task_params,
        content_lease.clone(),
    )
    .await;
    let exec_ms = start.elapsed().as_millis() as i64;
    let mut item_fields_count: i32 = 0;
    let mut item_status = "failed".to_string();
    let mut record_status = "failed".to_string();
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
                    Ok(true)
                }
                TranslateContentOutcome::NoChanges => {
                    item_status = "done".to_string();
                    record_status = "skipped".to_string();
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
            let project = |conn: &rusqlite::Connection| -> anyhow::Result<()> {
                crate::db::translations::insert_translation_record(
                    conn,
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
                )?;
                if let Some(jid) = job_id {
                    let (raw_path, translated_path) = item_disk_paths(&wp_base, relation.id, &item);
                    create_job_item_row(
                        conn,
                        jid,
                        &wp_base,
                        &relation,
                        &item,
                        &rules,
                        &task_params,
                        &execution_summary,
                        &raw_path,
                        &translated_path,
                        item_status.as_str(),
                        error_msg.as_deref(),
                    )
                    .map_err(|error| error.context("persist translation job item outcome"))?;
                }
                if let Some(completion) = &execution_summary.completion {
                    confirm_callback_in(
                        conn,
                        &wp_base,
                        &completion.identity,
                        &completion.payload,
                        &item,
                        &completion.ack,
                    )?;
                }
                Ok(())
            };
            if let Some(completion) = &execution_summary.completion {
                mutate_saved_callback(
                    db_arc,
                    &content_lease,
                    &wp_base,
                    &completion.identity,
                    &completion.payload,
                    &completion.authority,
                    project,
                )
                .await?;
            } else {
                mutate_content(db_arc, &content_lease, project).await?;
            }
        }
    }

    result
}

/// Translate a single content item and submit the result to WP.
///
/// Uses the persistent pipeline: translate -> persist to disk -> sync to WP.
/// Each step updates the item's status in DB, enabling crash recovery.
///
/// `job_id`: when the caller threaded a translation job (outbox / retry
/// fast paths), a job-item row is created for every outcome so the jobs
/// dashboard and the pending-review pool can see the work. Callers that do
/// their own row bookkeeping (the relation-scan owned path) pass `None`.
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
    job_id: Option<i64>,
    task_params: &DiscoveryTaskParams,
    content_lease: Option<Arc<crate::db::unit_lock::ContentExecution>>,
) -> anyhow::Result<(TranslateContentOutcome, usize, TranslationExecutionSummary)> {
    let db = match db {
        Some(db) => Some(db),
        None => Some(pending_store.lock().await.database()),
    };
    let defer_completion = content_lease.is_some();
    let _content_lease = match (db.as_ref(), content_lease) {
        (_, Some(lease)) => Some(lease),
        (Some(db), None) => Some(
            crate::db::unit_lock::ContentExecution::acquire(
                db,
                wp_base,
                relation.id,
                &item.object_type,
                item.object_id,
            )
            .await?,
        ),
        _ => None,
    };
    if item.needs_resync {
        if let Some(db) = db.as_ref() {
            anyhow::ensure!(
                find_pending_callback(
                    &*db.lock().await,
                    wp_base,
                    relation.id,
                    &item.object_type,
                    item.object_id
                )?
                .is_none(),
                "source changed while original callback is unconfirmed; retained"
            );
            restore_completed_callback_for_resync(
                db,
                &_content_lease,
                wp_base,
                relation.id,
                item,
                route_secret.map(str::to_string),
            )
            .await?;
        }
    }
    if let Some(db) = db.as_ref() {
        let pending = find_pending_callback(
            &*db.lock().await,
            wp_base,
            relation.id,
            &item.object_type,
            item.object_id,
        )?;
        if let Some(pending) = pending {
            let authority = capture_callback_authority(
                db,
                &_content_lease,
                wp_base,
                &pending.idempotency_key,
                &pending.payload,
                item,
            )
            .await?;
            let _callback = callback_sem
                .acquire()
                .await
                .context("callback budget is closed; retained")?;
            let _global = match global_callback_sem.as_ref() {
                Some(sem) => Some(
                    sem.clone()
                        .acquire_owned()
                        .await
                        .context("global callback budget is closed; retained")?,
                ),
                None => None,
            };
            adaptive.wait_turn().await;
            let outcome = match send_translation_callback(
                client,
                wp_base,
                token,
                &pending.payload.worker_id,
                &worker_config.device_id,
                &pending.idempotency_key,
                &pending.payload,
            )
            .await
            {
                Ok(ack) => {
                    confirm_callback(
                        db,
                        &_content_lease,
                        wp_base,
                        &pending.idempotency_key,
                        &pending.payload,
                        item,
                        &ack,
                        &authority,
                    )
                    .await?;
                    TranslateContentOutcome::Completed
                }
                Err(error) => {
                    adaptive.on_error_message(&format!("{error:#}"));
                    mutate_content(db, &_content_lease, |conn| {
                        increment_retry_pending_callback(
                            conn,
                            wp_base,
                            relation.id,
                            &item.object_type,
                            item.object_id,
                        )
                    })
                    .await?;
                    TranslateContentOutcome::PendingCallback
                }
            };
            let components = pending
                .payload
                .field_results
                .iter()
                .map(|field| field.provider_component.trim())
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect();
            return Ok((
                outcome,
                pending.payload.translated_fields.len(),
                build_translation_execution_summary(&pending.payload, components),
            ));
        }
        if let Some(saved) =
            super::receipts::completed(&*db.lock().await, wp_base, relation.id, item)?
        {
            let fields = saved.payload.translated_fields.len();
            let components = saved
                .payload
                .field_results
                .iter()
                .map(|field| field.provider_component.trim())
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect();
            return Ok((
                TranslateContentOutcome::Completed,
                fields,
                build_translation_execution_summary(&saved.payload, components),
            ));
        }
        crate::db::translations::has_materialized_success_record_checked(
            &*db.lock().await,
            wp_base,
            relation.id,
            item.object_id,
            normalize_object_type_key(&item.object_type),
        )?;
        anyhow::ensure!(
            !crate::db::jobs::has_pending_review_item_checked(
                &*db.lock().await,
                wp_base,
                relation.id,
                item.object_id
            )?,
            "saved pending review must be resolved before automatic translation; retained"
        );
    }
    // An unusable at-rest key is known before any provider can charge.
    crate::bindings::encrypt_for_save("{}")?;
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
    // FL-9 leak guard: an unbound task (no explicit component selection and
    // no env override) must not silently execute on a component that is the
    // explicit selection of a DIFFERENT (domain, relation) discovery task.
    // The anonymous fallback tier is narrowed to unclaimed components, so an
    // unbound task fast-fails (`no_component_for_format`) instead of
    // hijacking another relation's — possibly dead — component. Explicit
    // tiers (selected component, env override, payload hints, operator
    // bindings) stay unguarded; an empty claim set keeps legacy behavior
    // (single-component installs resolve exactly as before).
    let unbound_task_fallback =
        scoped_registry.is_none() && effective_component_id_override.trim().is_empty();
    let mut claimed_by_other_relations: Option<std::collections::HashSet<String>> = None;
    if unbound_task_fallback {
        if let Some(ref db_arc) = db {
            let conn = db_arc.lock().await;
            let claimed =
                crate::db::discovery_tasks::selected_components_claimed_by_other_relations(
                    &conn,
                    wp_base,
                    relation_id_i64,
                )?;
            if !claimed.is_empty() {
                let _ = log_event(
                    log_file,
                    "info",
                    "discovery.component_claim_guard_active",
                    json!({
                        "relation_id": relation_id_i64,
                        "object_id": item.object_id,
                        "claimed_component_count": claimed.len(),
                    }),
                );
                claimed_by_other_relations = Some(claimed);
            }
        }
    }
    let mut effective_relation = relation.clone();
    if let Some(ref source_lang) = task_params.effective_source_lang {
        effective_relation.source_lang = source_lang.clone();
    }
    if let Some(ref target_lang) = task_params.effective_target_lang {
        effective_relation.target_lang = target_lang.clone();
    }

    let _global_translate_permit = if let Some(sem) = global_translation_sem.as_ref() {
        Some(
            sem.clone()
                .acquire_owned()
                .await
                .context("translation budget is closed; retained")?,
        )
    } else {
        None
    };
    adaptive.wait_turn().await;
    // GAP-04 (tasks/client/06 §2.4, 批 I 2026-09-23): durable async-job
    // scope for this item — when the db is available, async provider jobs
    // (video/document, async_poll components) are persisted per
    // field×chunk×lane and resumed on the next re-offer after a restart.
    // CLI/no-db runs keep the pre-GAP-04 in-memory behavior (None).
    let async_scope = db
        .as_ref()
        .map(|db_arc| crate::db::async_jobs::AsyncJobScope {
            db: Arc::clone(db_arc),
            domain: wp_base.to_string(),
            relation_id: relation_id_i64,
            object_type: item.object_type.clone(),
            object_id: object_id_i64,
            source_snapshot: item.complete_data.get("__wptsall_job_snapshot").cloned(),
        });
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
        claimed_by_other_relations.as_ref(),
        async_scope,
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
            // Nothing to translate — still record the (terminal) pass on the
            // job so dashboards reflect the outbox/retry work item.
            if let (Some(ref db_arc), Some(jid)) = (&db, job_id) {
                let (raw_path, translated_path) = item_disk_paths(wp_base, relation.id, item);
                mutate_content(db_arc, &_content_lease, |conn| {
                    create_job_item_row(
                        conn,
                        jid,
                        wp_base,
                        relation,
                        item,
                        rules,
                        task_params,
                        &TranslationExecutionSummary::default(),
                        &raw_path,
                        &translated_path,
                        "done",
                        None,
                    )
                })
                .await?;
            }
            return Ok((
                TranslateContentOutcome::NoChanges,
                0,
                TranslationExecutionSummary::default(),
            ));
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
        upload_pending_media_with_optional_db(
            db.as_ref(),
            client,
            wp_base,
            token,
            &worker_config.worker_id,
            &worker_config.device_id,
            // Discovery path has no WP lifecycle task id yet; WP binds via
            // relation + source attachment. Do not pass relation_id here.
            0,
            relation_id_u64,
            &mut payload,
            log_file,
            route_secret,
        )
        .await?;
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
    let mut execution_summary =
        build_translation_execution_summary(&payload, actual_component_ids.clone());
    if let Some(ref db_arc) = db {
        let (raw_path, translated_path) = item_disk_paths(wp_base, relation.id, item);
        let conn = db_arc.lock().await;
        if let Some(owner) = _content_lease.as_ref() {
            owner.assert_owner(&conn, db_arc)?;
        }
        crate::task_engine::pipeline::save_immutable_json(
            std::path::Path::new(&raw_path),
            &item.complete_data,
        )
        .context("save discovery source snapshot")?;
        crate::task_engine::pipeline::save_immutable_json(
            std::path::Path::new(&translated_path),
            &json!({"idempotency_key":idempotency_key,"payload":payload}),
        )
        .context("save discovery result snapshot")?;
        if let Some(owner) = _content_lease.as_ref() {
            owner.assert_owner(&conn, db_arc)?;
        }
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
                mutate_content(db_arc, &_content_lease, |conn| {
                    // Outbox/retry callers threaded a job: create the row (in
                    // pending_review) so the review pool can see and approve it.
                    if let Some(jid) = job_id {
                        let (raw_path, translated_path) =
                            item_disk_paths(wp_base, relation.id, item);
                        create_job_item_row(
                            conn,
                            jid,
                            wp_base,
                            relation,
                            item,
                            rules,
                            task_params,
                            &execution_summary,
                            &raw_path,
                            &translated_path,
                            "pending_review",
                            None,
                        )?;
                    }
                    let items = crate::db::jobs::list_items_by_domain_object_checked(
                        conn,
                        wp_base,
                        relation_id_i64,
                        object_id_i64,
                    )?;
                    for it in &items {
                        if crate::db::pending_callbacks::normalize_pending_object_type(
                            &it.object_type,
                        )? == normalize_object_type_key(&item.object_type)
                        {
                            crate::db::jobs::update_item_status(
                                conn,
                                it.id,
                                "pending_review",
                                None,
                            )?;
                        }
                    }
                    Ok(())
                })
                .await?;
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
        mutate_content(db_arc, &_content_lease, |conn| {
            add_pending_callback(conn, &db_entry)
        })
        .await?;
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

    let callback_authority = match db.as_ref() {
        Some(db) => Some(
            capture_callback_authority(
                db,
                &_content_lease,
                wp_base,
                &idempotency_key,
                &payload,
                item,
            )
            .await?,
        ),
        None => None,
    };
    let _cb_permit = callback_sem
        .acquire()
        .await
        .context("callback budget is closed; retained")?;
    let _global_cb_permit = if let Some(sem) = global_callback_sem.as_ref() {
        Some(
            sem.clone()
                .acquire_owned()
                .await
                .context("global callback budget is closed; retained")?,
        )
    } else {
        None
    };
    adaptive.wait_turn().await;
    let cb_result = {
        let client_c = client.clone();
        let wp_base_c = wp_base.to_string();
        let token_c = token.to_string();
        let worker_id_c = worker_config.worker_id.clone();
        let device_id_c = worker_config.device_id.clone();
        let idempotency_key_c = idempotency_key.clone();
        let payload_c = payload.clone();
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
            adaptive.on_success();
            if defer_completion {
                execution_summary.completion = Some(CallbackCompletion {
                    identity: idempotency_key.clone(),
                    payload: payload.clone(),
                    ack,
                    authority: callback_authority.context("original callback authority missing")?,
                });
            } else if let Some(ref db_arc) = db {
                confirm_callback(
                    db_arc,
                    &_content_lease,
                    wp_base,
                    &idempotency_key,
                    &payload,
                    item,
                    &ack,
                    callback_authority
                        .as_ref()
                        .context("original callback authority missing")?,
                )
                .await?;
            } else {
                let store = pending_store.lock().await;
                anyhow::ensure!(
                    store.remove(
                        wp_base,
                        payload.relation_id,
                        normalize_object_type_key(&payload.object_type),
                        payload.object_id,
                    )?,
                    "original callback was not removed; retained"
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
            // The callback is queued for retry; the row reflects the
            // translated-but-not-yet-synced state.
            if let (Some(ref db_arc), Some(jid)) = (&db, job_id) {
                let (raw_path, translated_path) = item_disk_paths(wp_base, relation.id, item);
                mutate_content(db_arc, &_content_lease, |conn| {
                    create_job_item_row(
                        conn,
                        jid,
                        wp_base,
                        relation,
                        item,
                        rules,
                        task_params,
                        &execution_summary,
                        &raw_path,
                        &translated_path,
                        "translated",
                        None,
                    )
                })
                .await?;
            }
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

    // Fully completed (translated + callback accepted): the row lands as
    // done for the threaded job, if any.
    if let (Some(ref db_arc), Some(jid)) = (&db, job_id) {
        let (raw_path, translated_path) = item_disk_paths(wp_base, relation.id, item);
        mutate_content(db_arc, &_content_lease, |conn| {
            create_job_item_row(
                conn,
                jid,
                wp_base,
                relation,
                item,
                rules,
                task_params,
                &execution_summary,
                &raw_path,
                &translated_path,
                "done",
                None,
            )
        })
        .await?;
    }

    Ok((
        TranslateContentOutcome::Completed,
        fields_count,
        execution_summary,
    ))
}
