//! Content discovery flow: fetch site-relations, rules, and untranslated content
//! from a WP site, translate via component, and submit results via callback.
//!
//! This is an alternative to the task-pull flow (puller.rs). Enabled when
//! `WorkerConfig::discovery_mode` is `true`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Context;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use crate::auth::{wp_get_json_with_transport_and_secret, wp_request_with_transport};
use crate::component_rt::proxy::ProxyClientPool;
use crate::component_rt::runner::translate_text_with_constraints;
use crate::component_rt::selector::select_component_runtime_for_task_type;
use crate::config::{env_bool, env_u64, env_usize, DEFAULT_DATA_DIR};
use crate::db::discovery_tasks::{
    cleanup_expired_in_progress, ensure_discovery_tasks, get_discovery_task_params,
    release_in_progress, touch_last_run_at, try_claim_in_progress, DiscoveryTaskParams,
};
use crate::db::jobs::list_resumable_items;
use crate::db::pending_callbacks::{
    add_pending_callback, find_pending_callback, increment_retry_pending_callback,
    remove_pending_callback, PendingCallbackEntry,
};
use crate::logging::{log_event, snippet, unix_ts};
use crate::persistence::{PendingCallback, PendingCallbackStore};
use crate::task_engine::pipeline::{
    build_translated_path, persist_raw_content, persist_translated,
    sanitize_domain_key as pipeline_sanitize_domain_key, sync_i18n_item_to_wp, sync_item_to_wp,
    translate_item_fields_with_trace_using_proxy,
};
use crate::task_engine::submitter::{
    retry_with_backoff, send_i18n_translation_callback, send_translation_callback,
};
use crate::types::*;

mod adaptive;
mod execute;
mod fetch;
mod language_pack;
mod task_scope;
use self::adaptive::*;
use self::execute::*;
use self::fetch::*;
use self::language_pack::*;
use self::task_scope::*;

static MAX_ITEMS_PER_RUN_OVERRIDE: AtomicUsize = AtomicUsize::new(0);

pub struct MaxItemsPerRunOverrideGuard {
    previous: usize,
}

impl Drop for MaxItemsPerRunOverrideGuard {
    fn drop(&mut self) {
        MAX_ITEMS_PER_RUN_OVERRIDE.store(self.previous, Ordering::SeqCst);
    }
}

pub fn scoped_max_items_per_run_override(
    value: Option<usize>,
) -> Option<MaxItemsPerRunOverrideGuard> {
    value.map(|max_items| MaxItemsPerRunOverrideGuard {
        previous: MAX_ITEMS_PER_RUN_OVERRIDE.swap(max_items, Ordering::SeqCst),
    })
}

fn language_pack_source_text(complete_data: &LanguagePackCompleteData) -> &str {
    let plural_index = complete_data.plural_index.unwrap_or(0);
    if plural_index > 0 && !complete_data.msgid_plural.trim().is_empty() {
        return complete_data.msgid_plural.as_str();
    }
    if complete_data.msgid.trim().is_empty() && !complete_data.msgid_plural.trim().is_empty() {
        return complete_data.msgid_plural.as_str();
    }
    complete_data.msgid.as_str()
}

async fn drain_outbox_changes(
    client: &Client,
    wp_base: &str,
    token: &str,
    log_file: &str,
    worker_config: &WorkerConfig,
    route_secret: Option<&str>,
    relation_id: Option<i64>,
) -> Vec<OutboxContentChange> {
    let changes_url = match relation_id {
        Some(id) => format!("{}/content-changes?limit=100&relation_id={}", wp_base, id),
        None => format!("{}/content-changes?limit=100", wp_base),
    };
    match wp_get_json_with_transport_and_secret::<OutboxContentChangesResponse>(
        client,
        &changes_url,
        token,
        &worker_config.worker_id,
        route_secret,
    )
    .await
    {
        Ok(changes) => {
            let count = changes.data.items.len();
            let items = if changes.success {
                changes.data.items
            } else {
                Vec::new()
            };
            let _ = log_event(
                log_file,
                "info",
                "discovery.outbox_drained",
                json!({ "api_base_url": wp_base, "relation_id": relation_id, "count": count }),
            );
            items
        }
        Err(err) => {
            // Older installations do not expose the endpoint; discovery remains
            // fully compatible and will continue through the content API.
            let _ = log_event(
                log_file,
                "debug",
                "discovery.outbox_unavailable",
                json!({
                    "api_base_url": wp_base,
                    "relation_id": relation_id,
                    "error": snippet(&format!("{:#}", err))
                }),
            );
            Vec::new()
        }
    }
}

/// Discover and translate content for a single WP domain.
///
/// Flow:
/// 1. GET /client/site-relations -> list of relations
/// 2. For each relation: GET /client/rules?relation_id=X -> translation rules
/// 3. GET /client/content?relation_id=X&page=N -> untranslated content (paginated)
/// 4. Translate each content item via component runtime (concurrent pipelines)
/// 5. POST /client/translation-callback -> submit results
#[allow(clippy::too_many_arguments)]
pub(crate) async fn discover_and_translate(
    client: &Client,
    wp_base: &str,
    token: &str,
    log_file: &str,
    component_registry: Option<Arc<ComponentRuntimeRegistry>>,
    proxy_pool: Option<Arc<ProxyClientPool>>,
    component_id_override: &str,
    component_prefer_ids: &[String],
    task_type_component_bindings: Option<Arc<TaskTypeComponentBindingsDoc>>,
    rule_component_bindings: Option<&RuleComponentBindingsDoc>,
    worker_config: &WorkerConfig,
    route_secret: Option<&str>,
    pending_store: &Arc<Mutex<PendingCallbackStore>>,
    global_translation_sem: Option<Arc<Semaphore>>,
    global_callback_sem: Option<Arc<Semaphore>>,
    relation_limit: Option<usize>,
    // SQLite DB for discovery task params and in-progress dedup (None = CLI mode, use defaults).
    db: Option<Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
) -> anyhow::Result<DomainRunReport> {
    let domain_started_at = std::time::Instant::now();
    // CLI mode has no local discovery_tasks table, so keep the legacy domain-level
    // outbox drain for backward-compatible one-off workers. WebUI/Desktop (db Some)
    // drain after /site-relations and only for enabled relation-scoped tasks.
    let mut leased_outbox_changes: Vec<OutboxContentChange> = if db.is_none() {
        drain_outbox_changes(
            client,
            wp_base,
            token,
            log_file,
            worker_config,
            route_secret,
            None,
        )
        .await
    } else {
        Vec::new()
    };
    // Wrap rule_component_bindings in Arc so it can be cloned into async batch tasks.
    let rule_component_bindings_arc: Option<Arc<RuleComponentBindingsDoc>> =
        rule_component_bindings.map(|rb| Arc::new(rb.clone()));

    // Read callback concurrency, backlog throttle, and adaptive speed-control settings.
    let (
        callback_concurrency,
        relation_max_pending_callbacks,
        adaptive_rate_control,
        adaptive_max_delay_ms,
    ) = if let Some(ref db_arc) = db {
        let conn = db_arc.lock().await;
        let callback_concurrency =
            crate::db::system::get_system_config(&conn, "callback_concurrency")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(4);
        let relation_max_pending_callbacks =
            crate::db::system::get_system_config(&conn, "relation_max_pending_callbacks")
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(200)
                .max(1);
        let adaptive_rate_control =
            crate::db::system::get_system_config(&conn, "adaptive_rate_control")
                .map(|v| v == "true")
                .unwrap_or(true);
        let adaptive_max_delay_ms =
            crate::db::system::get_system_config(&conn, "adaptive_max_delay_ms")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(5000)
                .max(200);
        (
            callback_concurrency,
            relation_max_pending_callbacks,
            adaptive_rate_control,
            adaptive_max_delay_ms,
        )
    } else {
        let relation_max_pending_callbacks =
            std::env::var("WPTSALL_RELATION_MAX_PENDING_CALLBACKS")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(200)
                .max(1);
        let adaptive_rate_control = env_bool("WPTSALL_ADAPTIVE_RATE_CONTROL", true);
        let adaptive_max_delay_ms = env_u64("WPTSALL_ADAPTIVE_MAX_DELAY_MS", 5000).max(200);
        (
            4,
            relation_max_pending_callbacks,
            adaptive_rate_control,
            adaptive_max_delay_ms,
        )
    };
    let callback_sem = Arc::new(Semaphore::new(callback_concurrency.max(1)));
    let adaptive = Arc::new(AdaptiveRateControl::new(
        adaptive_rate_control,
        adaptive_max_delay_ms,
    ));
    let items_processed_total = Arc::new(AtomicUsize::new(0));
    let max_items_per_run_override = MAX_ITEMS_PER_RUN_OVERRIDE.load(Ordering::SeqCst);
    let max_items_per_run = if max_items_per_run_override > 0 {
        max_items_per_run_override
    } else {
        env_usize("WPTSALL_DISCOVERY_MAX_ITEMS_PER_RUN", 0)
    };

    // Extract domain name for DB keying (use wp_base as the key)
    let domain_key = wp_base.to_string();

    // 1. Fetch site relations
    let relations_url = format!("{}/site-relations", wp_base);
    adaptive.wait_turn().await;
    let relations_resp = wp_get_json_with_transport_and_secret::<RelationsResponse>(
        client,
        &relations_url,
        token,
        &worker_config.worker_id,
        route_secret,
    )
    .await
    .context("Failed to fetch site relations")?;
    adaptive.on_success();

    let mut relations = relations_resp.relations;
    if let Some(ref db_arc) = db {
        let mut relation_params_by_id: HashMap<i64, DiscoveryTaskParams> = HashMap::new();
        for relation in &relations {
            let params = {
                let conn = db_arc.lock().await;
                let _ = ensure_discovery_tasks(&conn, &domain_key, &[relation.id]);
                get_discovery_task_params(&conn, &domain_key, relation.id)
            };
            if !params.enabled {
                let _ = log_event(
                    log_file,
                    "info",
                    "discovery.relation_disabled",
                    json!({ "relation_id": relation.id, "domain": domain_key }),
                );
            }
            relation_params_by_id.insert(relation.id, params);
        }
        let fetched_count = relations.len();
        relations.retain(|relation| {
            relation_params_by_id
                .get(&relation.id)
                .map(|params| params.enabled)
                .unwrap_or(true)
        });
        if let Some(limit) = relation_limit {
            if limit > 0 && relations.len() > limit {
                relations.truncate(limit);
                let _ = log_event(
                    log_file,
                    "info",
                    "discovery.relations_limited",
                    json!({
                        "api_base_url": wp_base,
                        "limit": limit,
                        "fetched_count": fetched_count,
                        "enabled_count": relations.len(),
                    }),
                );
            }
        }
    } else if let Some(limit) = relation_limit {
        if limit > 0 && relations.len() > limit {
            relations.truncate(limit);
            let _ = log_event(
                log_file,
                "info",
                "discovery.relations_limited",
                json!({
                    "api_base_url": wp_base,
                    "limit": limit,
                }),
            );
        }
    }

    let _ = log_event(
        log_file,
        "info",
        "discovery.relations_fetched",
        json!({
            "api_base_url": wp_base,
            "count": relations.len()
        }),
    );

    let mut report = DomainRunReport {
        api_base_url: wp_base.to_string(),
        ..DomainRunReport::default()
    };

    if relations.is_empty() {
        // Do not strand a newly leased change merely because a relation was
        // removed between claim and discovery. Its WP lease will expire and it
        // will be retried after the relation is restored or cleaned up.
        return Ok(report);
    }

    if db.is_some() {
        for relation in &relations {
            let mut relation_changes = drain_outbox_changes(
                client,
                wp_base,
                token,
                log_file,
                worker_config,
                route_secret,
                Some(relation.id),
            )
            .await;
            leased_outbox_changes.append(&mut relation_changes);
        }
    }

    // Execute leased lifecycle events through the same component + callback
    // pipeline as normal discovery. This makes the outbox the authoritative
    // fast path instead of merely creating an unused compatibility task.
    for change in leased_outbox_changes {
        let Some(relation) = relations
            .iter()
            .find(|relation| relation.id == change.relation_id)
            .cloned()
        else {
            let _ = log_event(
                log_file,
                "warning",
                "discovery.outbox_relation_missing",
                json!({
                    "outbox_id": change.outbox_id, "relation_id": change.relation_id,
                }),
            );
            continue;
        };
        let rules_url = format!("{}/rules?relation_id={}", wp_base, relation.id);
        let rules = match wp_get_json_with_transport_and_secret::<RulesResponse>(
            client,
            &rules_url,
            token,
            &worker_config.worker_id,
            route_secret,
        )
        .await
        {
            Ok(response) => response.rules,
            Err(err) => {
                report.failed += 1;
                let _ = log_event(
                    log_file,
                    "warning",
                    "discovery.outbox_rules_failed",
                    json!({
                        "outbox_id": change.outbox_id, "task_id": change.task_id,
                        "error": snippet(&format!("{:#}", err)),
                    }),
                );
                continue;
            }
        };
        let task_params = if let Some(ref db_arc) = db {
            let conn = db_arc.lock().await;
            get_discovery_task_params(&conn, &domain_key, relation.id)
        } else {
            DiscoveryTaskParams::default()
        };
        let mut outbox_item = change.item;
        if let Some(object) = outbox_item.complete_data.as_object_mut() {
            object.insert(
                "_wptsall_outbox_id".to_string(),
                Value::from(change.outbox_id),
            );
        }
        let result = translate_content_item(
            client,
            wp_base,
            token,
            log_file,
            &outbox_item,
            &relation,
            &rules,
            component_registry.as_deref(),
            proxy_pool.as_deref(),
            component_id_override,
            component_prefer_ids,
            task_type_component_bindings.as_deref(),
            rule_component_bindings_arc.as_deref(),
            worker_config,
            route_secret,
            pending_store,
            db.clone(),
            &callback_sem,
            global_translation_sem.clone(),
            global_callback_sem.clone(),
            adaptive.clone(),
            &task_params,
        )
        .await;
        report.pulled += 1;
        report.processed += 1;
        match result {
            Ok((TranslateContentOutcome::Completed, _, _))
            | Ok((TranslateContentOutcome::PendingCallback, _, _)) => {
                report.completed += 1;
                let _ = log_event(
                    log_file,
                    "info",
                    "discovery.outbox_executed",
                    json!({
                        "outbox_id": change.outbox_id, "task_id": change.task_id,
                        "client_task_id": change.client_task_id,
                    }),
                );
            }
            Ok((TranslateContentOutcome::NoChanges, _, _))
            | Ok((TranslateContentOutcome::PendingReview, _, _)) => {
                // A no-op is terminal for this exact snapshot. Explicitly ack
                // it so an empty/untranslatable attachment or field does not
                // hold a lease until timeout.
                let ack_url = format!("{}/content-changes/{}/ack", wp_base, change.outbox_id);
                match wp_request_with_transport(
                    client,
                    reqwest::Method::POST,
                    &ack_url,
                    token,
                    &worker_config.worker_id,
                    &json!({ "outcome": "completed" }),
                    route_secret,
                )
                .await
                {
                    Ok(_) => report.completed += 1,
                    Err(_) => report.retried += 1,
                }
            }
            Err(err) => {
                report.failed += 1;
                let _ = log_event(
                    log_file,
                    "warning",
                    "discovery.outbox_execution_failed",
                    json!({
                        "outbox_id": change.outbox_id, "task_id": change.task_id,
                        "error": snippet(&format!("{:#}", err)),
                    }),
                );
            }
        }
    }

    // Clean up stale in-progress entries from previous runs.
    // Timeout is configurable via system_config key "in_progress_timeout_secs" (default 600).
    if let Some(ref db_arc) = db {
        {
            let conn = db_arc.lock().await;
            let timeout_secs =
                crate::db::system::get_system_config(&conn, "in_progress_timeout_secs")
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(1800);
            cleanup_expired_in_progress(&conn, timeout_secs);
        }
    }

    // One-time data lifecycle cleanup (orphaned files > 24h, old DB records > 7 days)
    {
        use std::sync::atomic::AtomicBool;
        static CLEANUP_DONE: AtomicBool = AtomicBool::new(false);
        if !CLEANUP_DONE.swap(true, Ordering::Relaxed) {
            let data_dir =
                std::env::var("WPTSALL_DATA_DIR").unwrap_or_else(|_| DEFAULT_DATA_DIR.to_string());
            crate::task_engine::pipeline::cleanup_orphaned_data_files(&data_dir, 24, log_file);
            if let Some(ref db_arc) = db {
                {
                    let conn = db_arc.lock().await;
                    let deleted = crate::db::translations::prune_old_records(&conn, 7).unwrap_or(0);
                    if deleted > 0 {
                        let _ = log_event(
                            log_file,
                            "info",
                            "lifecycle.records_pruned",
                            json!({ "deleted": deleted }),
                        );
                    }
                    let retries_deleted =
                        crate::db::translations::prune_old_retries(&conn, 7).unwrap_or(0);
                    if retries_deleted > 0 {
                        let _ = log_event(
                            log_file,
                            "info",
                            "lifecycle.retries_pruned",
                            json!({ "deleted": retries_deleted }),
                        );
                    }
                }
            }
        }
    }

    // Resume previously interrupted items (crash recovery).
    // Items stuck at "fetched" need re-translation; items at "translated" just need sync.
    if let Some(ref db_arc) = db {
        let pipeline_domain_key = pipeline_sanitize_domain_key(wp_base);
        let resumable = {
            let conn = db_arc.lock().await;
            list_resumable_items(&conn, &pipeline_domain_key, &["translated"])
        };
        if !resumable.is_empty() {
            let _ = log_event(
                log_file,
                "info",
                "discovery.resuming_items",
                json!({
                    "domain": domain_key,
                    "count": resumable.len(),
                }),
            );
            for item in &resumable {
                if item.status == "translated" && !item.translated_path.is_empty() {
                    // Re-sync: read payload from disk and submit callback
                    let sync_result =
                        if normalize_object_type_key(&item.object_type) == "language_pack" {
                            sync_i18n_item_to_wp(
                                db_arc,
                                client,
                                item.id,
                                &item.translated_path,
                                wp_base,
                                token,
                                worker_config,
                                route_secret,
                                log_file,
                                &callback_sem,
                            )
                            .await
                            .map(|count| count.max(1))
                        } else {
                            sync_item_to_wp(
                                db_arc,
                                client,
                                item.id,
                                &item.translated_path,
                                wp_base,
                                token,
                                worker_config,
                                route_secret,
                                log_file,
                                &callback_sem,
                            )
                            .await
                            .map(|_| 1usize)
                        };
                    match sync_result {
                        Ok(count) => {
                            report.completed += count;
                            report.processed += count;
                        }
                        Err(_) => {
                            let count = 1usize;
                            report.failed += count;
                            report.processed += count;
                        }
                    }
                }
            }
        }
    }

    // Read relation concurrency from DB (default 2, min 1)
    let relation_concurrency = if let Some(ref db_arc) = db {
        let conn = db_arc.lock().await;
        crate::db::system::get_system_config(&conn, "relation_concurrency")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1)
    } else {
        std::env::var("WPTSALL_RELATION_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1)
    }
    .max(1);

    let relation_sem = Arc::new(Semaphore::new(relation_concurrency));
    let mut relation_joins: JoinSet<(usize, usize, usize)> = JoinSet::new(); // (processed, completed, failed)

    let relations_count = relations.len();

    // 2. For each relation, get rules and content (concurrent, limited by relation_sem)
    for relation in relations {
        let _rel_permit = relation_sem.clone().acquire_owned().await;
        // Clone all values needed by the spawned task
        let client = client.clone();
        let wp_base = wp_base.to_string();
        let token = token.to_string();
        let log_file = log_file.to_string();
        let component_registry = component_registry.clone();
        let proxy_pool = proxy_pool.clone();
        let component_id_override = component_id_override.to_string();
        let component_prefer_ids = component_prefer_ids.to_vec();
        let task_type_component_bindings = task_type_component_bindings.clone();
        let rule_component_bindings_arc = rule_component_bindings_arc.clone();
        let worker_config = worker_config.clone();
        let route_secret = route_secret.map(|s| s.to_string());
        let pending_store = Arc::clone(pending_store);
        let db = db.clone();
        let domain_key = domain_key.clone();
        let items_processed_total = Arc::clone(&items_processed_total);
        let callback_sem = Arc::clone(&callback_sem);
        let global_translation_sem = global_translation_sem.clone();
        let global_callback_sem = global_callback_sem.clone();
        let adaptive = Arc::clone(&adaptive);
        let relation = relation.clone();

        relation_joins.spawn(async move {
        let relation = &relation;
        let relation_started_at = std::time::Instant::now();
        let route_secret = route_secret.as_deref();
        let mut rel_processed = 0usize;
        let mut rel_completed = 0usize;
        let mut rel_failed = 0usize;

        // Re-borrow owned values as references so the body code matches the
        // original (pre-spawn) types.  Nested spawns call .clone()/.to_string()
        // on these refs to obtain owned values for their own 'static blocks.
        let client = &client;
        let wp_base: &str = &wp_base;
        let token: &str = &token;
        let log_file: &str = &log_file;
        let component_id_override: &str = &component_id_override;
        let component_prefer_ids: &[String] = &component_prefer_ids;
        let worker_config = &worker_config;
        let pending_store = &pending_store;
        let callback_sem = &callback_sem;
        let rule_component_bindings = rule_component_bindings_arc.as_deref();

        // Ensure discovery task row exists + load per-relation params before any
        // WP per-relation calls. Disabled relations must not request rules,
        // content, or relation-scoped outbox rows.
        let params = if let Some(ref db_arc) = db {
            let conn = db_arc.lock().await;
            let _ = ensure_discovery_tasks(&conn, &domain_key, &[relation.id]);
            get_discovery_task_params(&conn, &domain_key, relation.id)
        } else {
            DiscoveryTaskParams::default()
        };

        if !params.enabled {
            let _ = log_event(
                log_file,
                "info",
                "discovery.relation_disabled",
                json!({ "relation_id": relation.id, "domain": domain_key }),
            );
            return (rel_processed, rel_completed, rel_failed);
        }

        let rules_url = format!("{}/rules?relation_id={}", wp_base, relation.id);
        adaptive.wait_turn().await;
        let rules = match wp_get_json_with_transport_and_secret::<RulesResponse>(
            client,
            &rules_url,
            token,
            &worker_config.worker_id,
            route_secret,
        )
        .await
        {
            Ok(resp) => {
                adaptive.on_success();
                resp.rules
            }
            Err(err) => {
                adaptive.on_error_message(&format!("{:#}", err));
                let _ = log_event(
                    log_file,
                    "error",
                    "discovery.rules_fetch_failed",
                    json!({
                        "relation_id": relation.id,
                        "error": snippet(&format!("{:#}", err))
                    }),
                );
                return (rel_processed, rel_completed, rel_failed);
            }
        };

        let _ = log_event(
            log_file,
            "info",
            "discovery.rules_fetched",
            json!({
                "relation_id": relation.id,
                "rule_count": rules.len()
            }),
        );

        // Backlog throttle: skip relation if too many pending callbacks are queued.
        let max_pending: i64 = relation_max_pending_callbacks.max(1);
        if let Some(ref db_arc) = db {
        let pending_count = {
            let conn = db_arc.lock().await;
            let pipeline_domain_key = pipeline_sanitize_domain_key(wp_base);
            crate::db::pending_callbacks::count_pending_for_relation(&conn, &domain_key, relation.id)
                + crate::db::jobs::count_items_by_status_for_relation(&conn, &pipeline_domain_key, relation.id, "translated")
        };
            if pending_count >= max_pending {
                let _ = log_event(
                    log_file,
                    "info",
                    "discovery.relation_throttled",
                    json!({
                        "relation_id": relation.id,
                        "pending_count": pending_count,
                        "max_pending": max_pending,
                    }),
                );
                return (rel_processed, rel_completed, rel_failed);
            }
        }

        let _ = log_event(
            log_file,
            "info",
            "discovery.relation_start",
            json!({
                "relation_id": relation.id,
                "concurrency": params.concurrency,
                "batch_parallel": params.batch_parallel,
                "per_page": params.per_page
            }),
        );

        // Create a TranslationJob in DB and mark as running (single lock)
        let job_id: Option<i64> = if let Some(ref db_arc) = db {
            let conn = db_arc.lock().await;
            let jid = crate::db::jobs::create_job(&conn, &crate::db::jobs::CreateJobRequest {
                domain: domain_key.clone(),
                relation_id: relation.id,
                business_line: "discovery".to_string(),
                triggered_by: "auto".to_string(),
            }).ok();
            if let Some(id) = jid {
                let _ = crate::db::jobs::update_job_status(&conn, id, "running");
            }
            jid
        } else {
            None
        };
        let job_id = Arc::new(job_id);

        // 2b. Process retry queue entries before normal discovery.
        //
        // Pending retries are fetched from the retry_queue table and translated
        // using the same pipeline. We fetch their content via `include_ids`
        // parameter so WP returns only the specific objects.
        if let Some(ref db_arc) = db {
            let retries = {
                let conn = db_arc.lock().await;
                crate::db::translations::get_pending_retries(&conn, &domain_key, relation.id)
            };
            if !retries.is_empty() {
                let mut retry_ids_by_type: std::collections::BTreeMap<&'static str, Vec<i64>> =
                    std::collections::BTreeMap::new();
                for retry in &retries {
                    let data_type = match retry.object_type.as_str() {
                        "term" | "taxonomy" => "term",
                        _ => "post",
                    };
                    retry_ids_by_type
                        .entry(data_type)
                        .or_default()
                        .push(retry.object_id);
                }

                for (retry_data_type, mut object_ids) in retry_ids_by_type {
                    object_ids.sort_unstable();
                    object_ids.dedup();
                    if object_ids.is_empty() {
                        continue;
                    }

                    let ids_param = object_ids
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    let retry_url = format!(
                        "{}/content?relation_id={}&data_type={}&include_ids={}",
                        wp_base, relation.id, retry_data_type, ids_param
                    );
                    let _ = log_event(
                        log_file,
                        "info",
                        "discovery.retry_queue_processing",
                        json!({
                            "relation_id": relation.id,
                            "data_type": retry_data_type,
                            "retry_count": object_ids.len(),
                            "object_ids": ids_param,
                        }),
                    );
                    adaptive.wait_turn().await;
                    match wp_get_json_with_transport_and_secret::<ContentResponse>(
                        client,
                        &retry_url,
                        token,
                        &worker_config.worker_id,
                        route_secret,
                    )
                    .await
                    {
                        Ok(content) => {
                            adaptive.on_success();
                            let _ = log_event(
                                log_file,
                                "info",
                                "discovery.retry_content_fetched",
                                json!({
                                    "relation_id": relation.id,
                                    "data_type": retry_data_type,
                                    "items": content.items.len(),
                                    "total": content.total,
                                    "per_page": content.per_page,
                                }),
                            );
                            for item in content.items {
                                let oid = item.object_id;
                                let object_type = item.object_type.clone();
                                let _ = log_event(
                                    log_file,
                                    "info",
                                    "discovery.retry_item_start",
                                    json!({
                                        "relation_id": relation.id,
                                        "object_id": oid,
                                        "object_type": object_type.as_str(),
                                        "data_type": retry_data_type,
                                    }),
                                );
                                let result = translate_content_item(
                                    client,
                                    wp_base,
                                    token,
                                    log_file,
                                    &item,
                                    relation,
                                    &rules,
                                    component_registry.as_deref(),
                                    proxy_pool.as_deref(),
                                    component_id_override,
                                    component_prefer_ids,
                                    task_type_component_bindings.as_deref(),
                                    rule_component_bindings,
                                    worker_config,
                                    route_secret,
                                    pending_store,
                                    db.clone(),
                                    callback_sem,
                                    global_translation_sem.clone(),
                                    global_callback_sem.clone(),
                                    Arc::clone(&adaptive),
                                    &params,
                                )
                                .await;
                                rel_processed += 1;
                                match result {
                                    Ok((outcome, fields_count, summary)) => {
                                        let outcome_name = match outcome {
                                            TranslateContentOutcome::Completed => "completed",
                                            TranslateContentOutcome::NoChanges => "no_changes",
                                            TranslateContentOutcome::PendingReview => {
                                                "pending_review"
                                            }
                                            TranslateContentOutcome::PendingCallback => {
                                                "pending_callback"
                                            }
                                        };
                                        let _ = log_event(
                                            log_file,
                                            "info",
                                            "discovery.retry_item_result",
                                            json!({
                                                "relation_id": relation.id,
                                                "object_id": oid,
                                                "object_type": object_type.as_str(),
                                                "data_type": retry_data_type,
                                                "outcome": outcome_name,
                                                "fields_count": fields_count,
                                                "component_ids": summary.component_ids,
                                                "failed_fields_count": summary.failed_fields_count,
                                                "primary_failure_reason": summary.primary_failure_reason,
                                            }),
                                        );
                                        if outcome == TranslateContentOutcome::Completed {
                                            rel_completed += 1;
                                            {
                                                let conn = db_arc.lock().await;
                                                let _ = crate::db::translations::mark_retry_done(
                                                    &conn,
                                                    &domain_key,
                                                    relation.id,
                                                    item.object_type.as_str(),
                                                    oid,
                                                );
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        rel_failed += 1;
                                        let _ = log_event(
                                            log_file,
                                            "warning",
                                            "discovery.retry_item_failed",
                                            json!({
                                                "relation_id": relation.id,
                                                "object_id": oid,
                                                "object_type": object_type.as_str(),
                                                "data_type": retry_data_type,
                                                "error": snippet(&format!("{:#}", err)),
                                            }),
                                        );
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            adaptive.on_error_message(&format!("{:#}", err));
                            let _ = log_event(
                                log_file,
                                "warning",
                                "discovery.retry_fetch_failed",
                                json!({
                                    "relation_id": relation.id,
                                    "data_type": retry_data_type,
                                    "error": snippet(&format!("{:#}", err)),
                                }),
                            );
                        }
                    }
                }
            }
        }

        // 3. Fetch untranslated content with concurrent batch pipelines.
        //
        // Content discovery runs per data_type ("post", "term"). This keeps
        // pagination and claim leases independent per object class.
        let content_data_types = discover_content_data_types(relation, &rules);
        let _ = log_event(
            log_file,
            "info",
            "discovery.content_data_types",
            json!({
                "relation_id": relation.id,
                "data_types": content_data_types,
            }),
        );

        for content_data_type in content_data_types {
            // batch_parallel pipelines share an AtomicI64 page counter so they each
            // fetch different pages for the current data_type.
            let page_counter = Arc::new(AtomicI64::new(1));
            let total_pages = Arc::new(AtomicI64::new(i64::MAX));

            let rules_arc = Arc::new(rules.clone());
            let mut batch_joins: JoinSet<(usize, usize, usize)> = JoinSet::new();

            for _ in 0..params.batch_parallel.max(1) {
                let client = client.clone();
                let wp_base = wp_base.to_string();
                let token = token.to_string();
                let log_file = log_file.to_string();
                let relation = relation.clone();
                let rules = Arc::clone(&rules_arc);
                let component_registry = component_registry.clone();
                let component_id_override = component_id_override.to_string();
                let component_prefer_ids = component_prefer_ids.to_vec();
                let task_type_component_bindings = task_type_component_bindings.clone();
                let rule_component_bindings_arc = rule_component_bindings_arc.clone();
                let worker_config = worker_config.clone();
                let route_secret_owned = route_secret.map(|s| s.to_string());
                let pending_store = Arc::clone(pending_store);
                let page_counter = Arc::clone(&page_counter);
                let total_pages = Arc::clone(&total_pages);
                let db = db.clone();
                let domain_key = domain_key.clone();
                let items_processed_total = Arc::clone(&items_processed_total);
                let callback_sem = Arc::clone(callback_sem);
                let global_translation_sem = global_translation_sem.clone();
                let global_callback_sem = global_callback_sem.clone();
                let adaptive = Arc::clone(&adaptive);
                let job_id = Arc::clone(&job_id);
                let params = params.clone();
                let content_data_type = content_data_type.to_string();
                let proxy_pool = proxy_pool.clone();

                batch_joins.spawn(async move {
                    let mut batch_completed = 0usize;
                    let mut batch_processed = 0usize;
                    let mut batch_failed = 0usize;

                    // Item-level semaphore shared across all pages for this batch pipeline.
                    // Created once per relation (outside the page loop) so concurrency is
                    // enforced across pages, not reset for each page.
                    let sem = Arc::new(Semaphore::new(params.concurrency.max(1) as usize));

                    loop {
                        if max_items_per_run > 0
                            && items_processed_total.load(Ordering::Relaxed) >= max_items_per_run
                        {
                            let _ = log_event(
                                &log_file,
                                "info",
                                "discovery.run_item_cap_reached",
                                json!({
                                    "relation_id": relation.id,
                                    "data_type": content_data_type,
                                    "max_items_per_run": max_items_per_run
                                }),
                            );
                            break;
                        }

                        let page = page_counter.fetch_add(1, Ordering::SeqCst);
                        if page > total_pages.load(Ordering::Acquire) {
                            break;
                        }

                        // Fetch content page
                        let content_url = if params.include_resync {
                            format!(
                                "{}/content?relation_id={}&data_type={}&page={}&per_page={}&include_resync=true",
                                wp_base, relation.id, content_data_type, page, params.per_page
                            )
                        } else {
                            format!(
                                "{}/content?relation_id={}&data_type={}&page={}&per_page={}",
                                wp_base, relation.id, content_data_type, page, params.per_page
                            )
                        };
                        adaptive.wait_turn().await;
                        let content = match wp_get_json_with_transport_and_secret::<ContentResponse>(
                            &client,
                            &content_url,
                            &token,
                            &worker_config.worker_id,
                            route_secret_owned.as_deref(),
                        )
                        .await
                        {
                            Ok(resp) => {
                                adaptive.on_success();
                                resp
                            }
                            Err(err) => {
                                adaptive.on_error_message(&format!("{:#}", err));
                                let _ = log_event(
                                    &log_file,
                                    "error",
                                    "discovery.content_fetch_failed",
                                    json!({
                                        "relation_id": relation.id,
                                        "data_type": content_data_type,
                                        "page": page,
                                        "error": snippet(&format!("{:#}", err))
                                    }),
                                );
                                break;
                            }
                        };

                        // Only lower total_pages, never raise it.
                        tighten_total_pages(&total_pages, content.total, content.per_page);

                        if content.items.is_empty() {
                            break;
                        }

                        let _ = log_event(
                            &log_file,
                            "info",
                            "discovery.content_page_fetched",
                            json!({
                                "relation_id": relation.id,
                                "data_type": content_data_type,
                                "page": page,
                                "items": content.items.len(),
                                "total": content.total
                            }),
                        );

                        // Claim content items before translating (30-minute lock).
                        // Current claim storage exists only for post/taxonomy lanes.
                        let mut content_items = content.items;
                        if matches!(content_data_type.as_str(), "post" | "term") {
                            let is_term_claim = content_data_type == "term";
                            let claim_items = build_content_claim_items(&content_items, is_term_claim);
                            let claim_url = format!("{}/content/claim", wp_base);
                            let claim_body = json!({
                                "relation_id": relation.id,
                                "data_type": content_data_type,
                                "items": claim_items
                            });
                            adaptive.wait_turn().await;
                            match wp_request_with_transport(
                                &client,
                                reqwest::Method::POST,
                                &claim_url,
                                &token,
                                &worker_config.worker_id,
                                &claim_body,
                                route_secret_owned.as_deref(),
                            )
                            .await
                            {
                                Ok(resp) => {
                                    adaptive.on_success();
                                    let (claimed_count, claimed_items_returned) =
                                        match serde_json::from_value::<ContentClaimResponse>(resp) {
                                        Ok(claim_resp) => {
                                            let claimed_count = claim_resp.claimed_count;
                                            let claimed_items_returned =
                                                claim_resp.claimed_items.as_ref().map(|v| v.len());
                                            if let Some(claimed_items) = claim_resp.claimed_items {
                                                let before = content_items.len();
                                                let claimed_keys = retain_claimed_content_items(
                                                    &mut content_items,
                                                    claimed_items,
                                                    is_term_claim,
                                                );
                                                let _ = log_event(
                                                    &log_file,
                                                    "info",
                                                    "discovery.content_claim_filtered",
                                                    json!({
                                                        "relation_id": relation.id,
                                                        "data_type": content_data_type,
                                                        "before": before,
                                                        "after": content_items.len(),
                                                        "claimed_items_returned": claimed_keys
                                                    }),
                                                );
                                                if content_items.is_empty() {
                                                    break;
                                                }
                                            } else if claim_resp.claimed_count == Some(0) {
                                                let _ = log_event(
                                                    &log_file,
                                                    "info",
                                                    "discovery.content_claim_zero",
                                                    json!({
                                                        "relation_id": relation.id,
                                                        "data_type": content_data_type,
                                                        "items": claim_items.len()
                                                    }),
                                                );
                                                break;
                                            } else {
                                                let _ = log_event(
                                                    &log_file,
                                                    "warning",
                                                    "discovery.content_claim_missing_items",
                                                    json!({
                                                        "relation_id": relation.id,
                                                        "data_type": content_data_type,
                                                        "claimed_count": claim_resp.claimed_count,
                                                        "items": claim_items.len()
                                                    }),
                                                );
                                                break;
                                            }
                                            (claimed_count, claimed_items_returned)
                                        }
                                        Err(err) => {
                                            let _ = log_event(
                                                &log_file,
                                                "warning",
                                                "discovery.content_claim_parse_failed",
                                                json!({
                                                    "relation_id": relation.id,
                                                    "data_type": content_data_type,
                                                    "error": snippet(&format!("{:#}", err))
                                                }),
                                            );
                                            break;
                                        }
                                    };
                                    let _ = log_event(
                                        &log_file,
                                        "info",
                                        "discovery.content_claimed",
                                        json!({
                                            "relation_id": relation.id,
                                            "data_type": content_data_type,
                                            "items": claim_items.len(),
                                            "claimed_count": claimed_count,
                                            "claimed_items_returned": claimed_items_returned
                                        }),
                                    );
                                }
                                Err(err) => {
                                    adaptive.on_error_message(&format!("{:#}", err));
                                    let err_str = format!("{:#}", err);
                                    let _ = log_event(
                                        &log_file,
                                        "warning",
                                        "discovery.content_claim_failed",
                                        json!({
                                            "relation_id": relation.id,
                                            "data_type": content_data_type,
                                            "items": claim_items.len(),
                                            "error": snippet(&err_str)
                                        }),
                                    );
                                    // Do not submit items when the durable claim request failed.
                                    content_items.clear();
                                    break;
                                }
                            }
                        } else {
                            let _ = log_event(
                                &log_file,
                                "info",
                                "discovery.content_claim_skipped",
                                json!({
                                    "relation_id": relation.id,
                                    "data_type": content_data_type,
                                    "reason": "claim_not_required_for_data_type"
                                }),
                            );
                        }

                        // Spawn concurrent item translators with shared semaphore (created once per
                        // batch pipeline outside this page loop, so concurrency spans all pages).
                        let mut item_joins: JoinSet<(bool, bool)> = JoinSet::new(); // (completed, failed)

                        for item in content_items {
                            if max_items_per_run > 0
                                && items_processed_total.load(Ordering::Relaxed) >= max_items_per_run
                            {
                                let _ = log_event(
                                    &log_file,
                                    "info",
                                    "discovery.run_item_cap_reached",
                                    json!({
                                        "relation_id": relation.id,
                                        "data_type": content_data_type,
                                        "max_items_per_run": max_items_per_run
                                    }),
                                );
                                break;
                            }

                            items_processed_total.fetch_add(1, Ordering::Relaxed);

                            // In-progress dedup: skip if another pipeline is already translating this item.
                            // On DB lock failure, log and skip (false) rather than double-translate.
                            let claimed_local = if let Some(ref db_arc) = db {
                                let conn = db_arc.lock().await;
                                try_claim_in_progress(
                                    &conn,
                                    &domain_key,
                                    relation.id,
                                    item.object_type.as_str(),
                                    item.object_id,
                                )
                            } else {
                                true
                            };
                            if !claimed_local {
                                items_processed_total.fetch_sub(1, Ordering::Relaxed);
                                continue;
                            }

                            let permit = match sem.clone().acquire_owned().await {
                                Ok(p) => p,
                                Err(_) => {
                                    if let Some(ref db_arc) = db {
                                        let conn = db_arc.lock().await;
                                        release_in_progress(
                                            &conn,
                                            &domain_key,
                                            relation.id,
                                            item.object_type.as_str(),
                                            item.object_id,
                                        );
                                    }
                                    break;
                                }
                            };

                            let client = client.clone();
                            let wp_base = wp_base.clone();
                            let token = token.clone();
                            let log_file = log_file.clone();
                            let relation = relation.clone();
                            let rules = Arc::clone(&rules);
                            let component_registry = component_registry.clone();
                            let proxy_pool = proxy_pool.clone();
                            let component_id_override = component_id_override.clone();
                            let component_prefer_ids = component_prefer_ids.clone();
                            let task_type_component_bindings = task_type_component_bindings.clone();
                            let rule_component_bindings_item = rule_component_bindings_arc.clone();
                            let worker_config = worker_config.clone();
                            let route_secret_owned = route_secret_owned.clone();
                            let pending_store = Arc::clone(&pending_store);
                            let db = db.clone();
                            let domain_key = domain_key.clone();
                            let object_id = item.object_id;
                            let object_type = item.object_type.clone();
                            let callback_sem = Arc::clone(&callback_sem);
                            let global_translation_sem = global_translation_sem.clone();
                            let global_callback_sem = global_callback_sem.clone();
                            let adaptive = Arc::clone(&adaptive);
                            let job_id_val = *job_id;
                            let task_params = params.clone();

                            let relation_id_for_release = relation.id;
                            item_joins.spawn(async move {
                                let _permit = permit;
                                let result = translate_content_item_owned(
                                    client,
                                    wp_base,
                                    token,
                                    log_file,
                                    item,
                                    relation,
                                    rules,
                                    component_registry,
                                    proxy_pool,
                                    component_id_override,
                                    component_prefer_ids,
                                    task_type_component_bindings,
                                    rule_component_bindings_item,
                                    worker_config,
                                    route_secret_owned,
                                    pending_store,
                                    db.clone(),
                                    callback_sem,
                                    global_translation_sem,
                                    global_callback_sem,
                                    adaptive,
                                    job_id_val,
                                    task_params,
                                )
                                .await;

                                // Always release in-progress lock
                                if let Some(ref db_arc) = db {
                                    let conn = db_arc.lock().await;
                                    release_in_progress(
                                        &conn,
                                        &domain_key,
                                        relation_id_for_release,
                                        object_type.as_str(),
                                        object_id,
                                    );
                                }

                                match result {
                                    Ok(true) => (true, false),
                                    Ok(false) => (false, false),
                                    Err(_) => (false, true),
                                }
                            });
                        }

                        // Collect item results
                        while let Some(join_result) = item_joins.join_next().await {
                            match join_result {
                                Ok((completed, failed)) => {
                                    batch_processed += 1;
                                    if completed {
                                        batch_completed += 1;
                                    }
                                    if failed {
                                        batch_failed += 1;
                                    }
                                }
                                Err(_) => {
                                    batch_processed += 1;
                                    batch_failed += 1;
                                }
                            }
                        }
                    }

                    (batch_completed, batch_processed, batch_failed)
                });
            }

            // Collect batch pipeline results for this data_type
            while let Some(join_result) = batch_joins.join_next().await {
                match join_result {
                    Ok((c, p, f)) => {
                        rel_completed += c;
                        rel_processed += p;
                        rel_failed += f;
                    }
                    Err(_) => {
                        rel_failed += 1;
                        rel_processed += 1;
                    }
                }
            }
        }

        // ── Language pack discovery (v1.3.0+) ──────────────────────────────
        // If i18n_config signals language pack translation is enabled, do a
        // separate content fetch loop with data_type=language_pack.
        let translate_plugin = relation
            .i18n_config
            .as_ref()
            .map(|c| c.translate_plugin_i18n)
            .unwrap_or(false);
        let translate_theme = relation
            .i18n_config
            .as_ref()
            .map(|c| c.translate_theme_i18n)
            .unwrap_or(false);
        let translate_config = relation
            .i18n_config
            .as_ref()
            .map(|c| c.translate_config_i18n)
            .unwrap_or(false);
        let translate_site = relation
            .i18n_config
            .as_ref()
            .map(|c| c.translate_site_strings)
            .unwrap_or(false);
        let translate_menu = relation
            .i18n_config
            .as_ref()
            .map(|c| c.translate_menu_strings)
            .unwrap_or(false);
        let translate_widget = relation
            .i18n_config
            .as_ref()
            .map(|c| c.translate_widget_strings)
            .unwrap_or(false);

        for (business_line, subtype, enabled, data_type) in [
            ("plugin_i18n", "plugin", translate_plugin, "language_pack"),
            ("theme_i18n", "theme", translate_theme, "language_pack"),
            ("config_i18n", "config", translate_config, "language_pack"),
            ("site_strings", "site", translate_site, "site_string"),
            ("menu_strings", "menu", translate_menu, "site_string"),
            ("widget_strings", "widget", translate_widget, "site_string"),
        ] {
            if !enabled {
                continue;
            }

            let _ = log_event(
                log_file,
                "info",
                "discovery.language_pack_start",
                json!({
                    "relation_id": relation.id,
                    "business_line": business_line,
                    "subtype": subtype
                }),
            );

            // Always request page=1: claimed entries are removed from the unclaimed pool,
            // so the next page=1 request returns the next batch.
            let mut lp_batch = 0u64;
            loop {
                if max_items_per_run > 0
                    && items_processed_total.load(Ordering::Relaxed) >= max_items_per_run
                {
                    let _ = log_event(
                        log_file,
                        "info",
                        "discovery.run_item_cap_reached",
                        json!({
                            "relation_id": relation.id,
                            "data_type": "language_pack",
                            "max_items_per_run": max_items_per_run,
                            "business_line": business_line,
                            "subtype": subtype
                        }),
                    );
                    break;
                }

                lp_batch += 1;
                let lp_url = format!(
                    "{}/content?relation_id={}&data_type={}&subtype={}&page=1&per_page={}",
                    wp_base, relation.id, data_type, subtype, params.per_page
                );
                adaptive.wait_turn().await;
                let lp_content = match wp_get_json_with_transport_and_secret::<LanguagePackContentResponse>(
                    client,
                    &lp_url,
                    token,
                    &worker_config.worker_id,
                    route_secret,
                )
                .await
                {
                    Ok(resp) => {
                        adaptive.on_success();
                        resp
                    }
                    Err(err) => {
                        adaptive.on_error_message(&format!("{:#}", err));
                        let _ = log_event(
                            log_file,
                            "error",
                            "discovery.language_pack_fetch_failed",
                            json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype,
                                "batch": lp_batch,
                                "error": snippet(&format!("{:#}", err))
                            }),
                        );
                        break;
                    }
                };

                if lp_content.items.is_empty() {
                    break;
                }

                let _ = log_event(
                    log_file,
                    "info",
                    "discovery.language_pack_page_fetched",
                    json!({
                        "relation_id": relation.id,
                        "business_line": business_line,
                        "subtype": subtype,
                        "batch": lp_batch,
                        "items": lp_content.items.len(),
                        "total": lp_content.total
                    }),
                );

                // Claim language pack items before translating
                let mut lp_items = lp_content.items;
                let claim_items = build_language_pack_claim_items(&lp_items);
                let claim_url = format!("{}/content/claim", wp_base);
                let claim_body = json!({
                    "relation_id": relation.id,
                    "data_type": data_type,
                    "subtype": subtype,
                    "items": claim_items
                });
                adaptive.wait_turn().await;
                match wp_request_with_transport(
                    client,
                    reqwest::Method::POST,
                    &claim_url,
                    token,
                    &worker_config.worker_id,
                    &claim_body,
                    route_secret,
                )
                .await
                {
                    Ok(resp) => {
                        adaptive.on_success();
                        let (claimed_count, claimed_items_returned) =
                            match serde_json::from_value::<ContentClaimResponse>(resp) {
                            Ok(claim_resp) => {
                                let claimed_count = claim_resp.claimed_count;
                                let claimed_items_returned =
                                    claim_resp.claimed_items.as_ref().map(|v| v.len());
                                if let Some(claimed_items) = claim_resp.claimed_items {
                                    let before = lp_items.len();
                                    let claimed_entry_ids =
                                        retain_claimed_language_pack_items(
                                            &mut lp_items,
                                            claimed_items,
                                        );
                                    let _ = log_event(
                                        log_file,
                                        "info",
                                        "discovery.language_pack_claim_filtered",
                                        json!({
                                            "relation_id": relation.id,
                                            "business_line": business_line,
                                            "subtype": subtype,
                                            "before": before,
                                            "after": lp_items.len(),
                                            "claimed_items_returned": claimed_entry_ids
                                        }),
                                    );
                                    if lp_items.is_empty() {
                                        break;
                                    }
                                } else if claim_resp.claimed_count == Some(0) {
                                    let _ = log_event(
                                        log_file,
                                        "info",
                                        "discovery.language_pack_claim_zero",
                                        json!({
                                            "relation_id": relation.id,
                                            "business_line": business_line,
                                            "subtype": subtype,
                                            "items": claim_items.len()
                                        }),
                                    );
                                    break;
								} else {
									let _ = log_event(
										log_file,
										"warning",
										"discovery.language_pack_claim_missing_items",
										json!({
											"relation_id": relation.id,
											"business_line": business_line,
											"subtype": subtype,
											"claimed_count": claim_resp.claimed_count,
											"items": claim_items.len()
										}),
									);
									break;
                                }
                                (claimed_count, claimed_items_returned)
                            }
                            Err(err) => {
                                let _ = log_event(
                                    log_file,
                                    "warning",
                                    "discovery.language_pack_claim_parse_failed",
                                    json!({
                                        "relation_id": relation.id,
                                        "business_line": business_line,
                                        "subtype": subtype,
                                        "error": snippet(&format!("{:#}", err))
                                    }),
                                );
								break;
                            }
                        };
                        let _ = log_event(
                            log_file,
                            "info",
                            "discovery.language_pack_claimed",
                            json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype,
                                "items": claim_items.len(),
                                "claimed_count": claimed_count,
                                "claimed_items_returned": claimed_items_returned
                            }),
                        );
                    }
                    Err(err) => {
                        adaptive.on_error_message(&format!("{:#}", err));
                        let err_str = format!("{:#}", err);
                        let _ = log_event(
                            log_file,
                            "warning",
                            "discovery.language_pack_claim_failed",
                            json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype,
                                "error": snippet(&err_str)
                            }),
                        );
                        // Do not submit unclaimed entries.
                        lp_items.clear();
                        break;
                    }
                }

                let scoped_registry = match build_task_scoped_runtime_registry(
                    component_registry.as_deref(),
                    params.selected_component_id.as_deref(),
                    params.editable_overrides.as_ref(),
                ) {
                    Ok(registry) => registry,
                    Err(err) => {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "discovery.language_pack_task_params_invalid",
                            json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype,
                                "error": snippet(&format!("{:#}", err))
                            }),
                        );
                        rel_failed += lp_items.len();
                        rel_processed += lp_items.len();
                        continue;
                    }
                };
                let effective_registry = scoped_registry.as_ref().or(component_registry.as_deref());
                let effective_component_id_override = params
                    .selected_component_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(component_id_override);
                let effective_relation = apply_discovery_task_relation_overrides(relation, &params);

                // Select component for this business_line
                let selection_payload = json!({
                    "__input_artifact_kind": "i18n_bundle",
                    "__expected_output_artifact_kind": "translated_i18n_bundle"
                });
                let component = select_component_runtime_for_task_type(
                    effective_registry,
                    &selection_payload,
                    business_line,
                    "text",
                    effective_component_id_override,
                    component_prefer_ids,
                    task_type_component_bindings.as_deref(),
                );
                let comp = match component {
                    Some(c) => c,
                    None => {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "discovery.language_pack_no_component",
                            json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype
                            }),
                        );
                        break;
                    }
                };

                let constraints = EffectiveConstraints::resolve(
                    comp.template.constraints.as_ref(),
                    None,
                    None,
                    worker_config.default_max_input_chars,
                    &worker_config.default_split_strategy,
                );
                let component_client = proxy_pool
                    .as_ref()
                    .map(|pool| pool.get_client(comp.proxy_profile_id.as_deref()))
                    .unwrap_or(client);

                // Translate each entry's msgid
                let mut translated_entries: Vec<I18nCallbackEntry> = Vec::new();
                for item in &lp_items {
                    if max_items_per_run > 0
                        && items_processed_total.load(Ordering::Relaxed) >= max_items_per_run
                    {
                        let _ = log_event(
                            log_file,
                            "info",
                            "discovery.run_item_cap_reached",
                            json!({
                                "relation_id": relation.id,
                                "data_type": "language_pack",
                                "max_items_per_run": max_items_per_run,
                                "business_line": business_line,
                                "subtype": subtype
                            }),
                        );
                        break;
                    }

                    items_processed_total.fetch_add(1, Ordering::Relaxed);

                    let cd = &item.complete_data;
                    let source_text = language_pack_source_text(cd);
                    if source_text.trim().is_empty() {
                        continue;
                    }
                    let _global_translate_permit = if let Some(sem) = global_translation_sem.as_ref()
                    {
                        sem.clone().acquire_owned().await.ok()
                    } else {
                        None
                    };
                    adaptive.wait_turn().await;
                    match translate_text_with_constraints(
                        component_client,
                        comp,
                        source_text,
                        &effective_relation.source_lang,
                        &effective_relation.target_lang,
                        &constraints,
                    )
                    .await
                    {
                        Ok(msgstr) => {
                            adaptive.on_success();
                            translated_entries.push(I18nCallbackEntry {
                                entry_id: cd.entry_id,
                                msgstr,
                            });
                        }
                        Err(err) => {
                            adaptive.on_error_message(&format!("{:#}", err));
                            let _ = log_event(
                                log_file,
                                "warning",
                                "discovery.language_pack_translate_failed",
                                json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype,
                                "entry_id": cd.entry_id,
                                "error": snippet(&format!("{:#}", err))
                            }),
                            );
                            rel_failed += 1;
                            rel_processed += 1;
                        }
                    };
                }

                if translated_entries.is_empty() {
                    continue;
                }

                if worker_config.review_mode {
                    if let (Some(ref db_arc), Some(jid)) = (&db, *job_id) {
                        let data_dir = crate::config::env_or("WPTSALL_DATA_DIR", DEFAULT_DATA_DIR);
                        let mut queued_for_review = 0usize;
                        for translated in &translated_entries {
                            let idempotency_key = format!(
                                "lang-pack-{}-{}-{}-{}-{}",
                                crate::bindings::normalize_domain_base(wp_base),
                                relation.id,
                                business_line,
                                translated.entry_id,
                                worker_config.worker_id
                            );
                            let i18n_payload = I18nCallbackPayload {
                                business_line: business_line.to_string(),
                                relation_id: u64::try_from(relation.id).unwrap_or(0),
                                client_task_id: idempotency_key.clone(),
                                worker_id: worker_config.worker_id.clone(),
                                source_lang: effective_relation.source_lang.clone(),
                                target_lang: effective_relation.target_lang.clone(),
                                entries: vec![translated.clone()],
                            };
                            let raw_path = format!(
                                "{}/raw/{}/rel_{}/language_pack_{}_{}_{}.json",
                                data_dir, domain_key, relation.id, business_line, subtype, translated.entry_id
                            );
                            let translated_path = format!(
                                "{}/translated/{}/rel_{}/language_pack_{}_{}_{}.json",
                                data_dir, domain_key, relation.id, business_line, subtype, translated.entry_id
                            );
                            let source_item = lp_items
                                .iter()
                                .find(|it| it.complete_data.entry_id == translated.entry_id);
                            let raw_json = json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype,
                                "entry_id": translated.entry_id,
                                "source_lang": effective_relation.source_lang.clone(),
                                "target_lang": effective_relation.target_lang.clone(),
                                "source": {
                                    "object_id": source_item.map(|it| it.object_id).unwrap_or(0),
                                    "text_domain": source_item.map(|it| it.complete_data.text_domain.clone()).unwrap_or_default(),
                                    "msgctxt": source_item.map(|it| it.complete_data.msgctxt.clone()).unwrap_or_default(),
                                    "msgid": source_item.map(|it| it.complete_data.msgid.clone()).unwrap_or_default(),
                                }
                            });
                            let envelope = I18nTranslatedEnvelope {
                                payload_type: "i18n_language_pack".to_string(),
                                idempotency_key: idempotency_key.clone(),
                                route_secret: route_secret.map(|s| s.to_string()),
                                payload: i18n_payload,
                                persisted_at: unix_ts() as i64,
                            };
                            let write_result: anyhow::Result<()> = (|| {
                                if let Some(parent) = std::path::Path::new(&raw_path).parent() {
                                    std::fs::create_dir_all(parent).with_context(|| {
                                        format!("create raw dir failed: {}", parent.display())
                                    })?;
                                }
                                if let Some(parent) = std::path::Path::new(&translated_path).parent()
                                {
                                    std::fs::create_dir_all(parent).with_context(|| {
                                        format!("create translated dir failed: {}", parent.display())
                                    })?;
                                }
                                let raw_str = serde_json::to_string_pretty(&raw_json)
                                    .context("serialize language_pack raw json failed")?;
                                std::fs::write(&raw_path, raw_str).with_context(|| {
                                    format!("write language_pack raw file failed: {}", raw_path)
                                })?;
                                let envelope_str = serde_json::to_string_pretty(&envelope)
                                    .context("serialize language_pack translated envelope failed")?;
                                std::fs::write(&translated_path, envelope_str).with_context(|| {
                                    format!(
                                        "write language_pack translated file failed: {}",
                                        translated_path
                                    )
                                })?;
                                Ok(())
                            })();
                            if let Err(err) = write_result {
                                let _ = log_event(
                                    log_file,
                                    "warning",
                                    "discovery.language_pack_review_persist_failed",
                                    json!({
                                        "relation_id": relation.id,
                                        "business_line": business_line,
                                        "subtype": subtype,
                                        "entry_id": translated.entry_id,
                                        "error": snippet(&format!("{:#}", err))
                                    }),
                                );
                                rel_failed += 1;
                                rel_processed += 1;
                                continue;
                            }
                            let item_result: anyhow::Result<()> = {
                                let conn = db_arc.lock().await;
                                match crate::db::jobs::create_item(
                                    &conn,
                                    &crate::db::jobs::CreateItemRequest {
                                        job_id: jid,
                                        domain: wp_base.to_string(),
                                        relation_id: relation.id,
                                        business_line: business_line.to_string(),
                                        object_type: "language_pack".to_string(),
                                        wp_object_id: translated.entry_id,
                                        wp_object_subtype: subtype.to_string(),
                                        task_type: "text".to_string(),
                                        source_lang: effective_relation.source_lang.clone(),
                                        target_lang: effective_relation.target_lang.clone(),
                                        component_id: comp.template.id.clone(),
                                        component_ids: vec![comp.template.id.clone()],
                                        selected_component_id: params
                                            .selected_component_id
                                            .clone()
                                            .or_else(|| Some(comp.template.id.clone())),
                                        effective_source_lang: Some(
                                            effective_relation.source_lang.clone(),
                                        ),
                                        effective_target_lang: Some(
                                            effective_relation.target_lang.clone(),
                                        ),
                                        editable_overrides: params.editable_overrides.clone(),
                                        raw_path: raw_path.clone(),
                                        client_task_id: idempotency_key.clone(),
                                        max_retries: 2,
                                    },
                                ) {
                                    Ok(item_id) => {
                                        if let Err(err) = crate::db::jobs::update_item_translated_path(
                                            &conn,
                                            item_id,
                                            &translated_path,
                                        ) {
                                            Err(err)
                                        } else if let Err(err) = crate::db::jobs::update_item_status(
                                            &conn,
                                            item_id,
                                            "pending_review",
                                            None,
                                        ) {
                                            Err(err)
                                        } else {
                                            Ok(())
                                        }
                                    }
                                    Err(err) => Err(err),
                                }
                            };
                            if let Err(err) = item_result {
                                let _ = log_event(
                                    log_file,
                                    "warning",
                                    "discovery.language_pack_review_item_failed",
                                    json!({
                                        "relation_id": relation.id,
                                        "business_line": business_line,
                                        "subtype": subtype,
                                        "entry_id": translated.entry_id,
                                        "error": snippet(&format!("{:#}", err))
                                    }),
                                );
                                rel_failed += 1;
                                rel_processed += 1;
                                continue;
                            }
                            queued_for_review += 1;
                        }
                        if queued_for_review > 0 {
                            let _ = log_event(
                                log_file,
                                "info",
                                "discovery.language_pack_pending_review",
                                json!({
                                    "relation_id": relation.id,
                                    "business_line": business_line,
                                    "subtype": subtype,
                                    "entries": queued_for_review
                                }),
                            );
                            rel_processed += queued_for_review;
                        }
                        continue;
                    } else {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "discovery.language_pack_review_mode_unavailable",
                            json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype,
                                "reason": "db/job unavailable; falling back to immediate callback"
                            }),
                        );
                    }
                }

                // Submit Path B callback (one per batch)
                let idempotency_key = language_pack_batch_idempotency_key(
                    wp_base,
                    relation.id,
                    business_line,
                    subtype,
                    &effective_relation.source_lang,
                    &effective_relation.target_lang,
                    &worker_config.worker_id,
                    &translated_entries,
                );
                let i18n_payload = I18nCallbackPayload {
                    business_line: business_line.to_string(),
                    relation_id: u64::try_from(relation.id).unwrap_or(0),
                    client_task_id: idempotency_key.clone(),
                    worker_id: worker_config.worker_id.clone(),
                    source_lang: effective_relation.source_lang.clone(),
                    target_lang: effective_relation.target_lang.clone(),
                    entries: translated_entries.clone(),
                };
                let i18n_result = if let (Some(ref db_arc), Some(jid)) = (&db, *job_id) {
                    match persist_language_pack_batch_for_sync(
                        db_arc,
                        jid,
                        &crate::config::env_or("WPTSALL_DATA_DIR", DEFAULT_DATA_DIR),
                        &domain_key,
                        wp_base,
                        relation,
                        &effective_relation,
                        business_line,
                        subtype,
                        &lp_items,
                        &translated_entries,
                        &idempotency_key,
                        route_secret,
                        &i18n_payload,
                        &comp.template.id,
                        &params,
                    )
                    .await
                    {
                        Ok(persisted) => {
                            let _global_cb_permit = if let Some(sem) = global_callback_sem.as_ref() {
                                sem.clone().acquire_owned().await.ok()
                            } else {
                                None
                            };
                            adaptive.wait_turn().await;
                            sync_i18n_item_to_wp(
                                db_arc,
                                client,
                                persisted.item_id,
                                &persisted.translated_path,
                                wp_base,
                                token,
                                worker_config,
                                route_secret,
                                log_file,
                                callback_sem,
                            )
                            .await
                            .map(|_| ())
                        }
                        Err(err) => Err(err),
                    }
                } else {
                    let _cb_permit = callback_sem.acquire().await.ok();
                    let _global_cb_permit = if let Some(sem) = global_callback_sem.as_ref() {
                        sem.clone().acquire_owned().await.ok()
                    } else {
                        None
                    };
                    adaptive.wait_turn().await;
                    retry_with_backoff(
                        "discovery.language_pack_callback",
                        relation.id,
                        log_file,
                        worker_config,
                        |_attempt| {
                            let client = client.clone();
                            let wp_base = wp_base.to_string();
                            let token = token.to_string();
                            let worker_id = worker_config.worker_id.clone();
                            let idempotency_key = idempotency_key.clone();
                            let i18n_payload = i18n_payload.clone();
                            let route_secret_owned = route_secret.map(|s| s.to_string());
                            async move {
                                send_i18n_translation_callback(
                                    &client,
                                    &wp_base,
                                    &token,
                                    &worker_id,
                                    &idempotency_key,
                                    &i18n_payload,
                                    route_secret_owned.as_deref(),
                                )
                                .await
                            }
                        },
                    )
                    .await
                    .map(|_| ())
                };
                match i18n_result {
                    Ok(_) => {
                        adaptive.on_success();
                        let count = translated_entries.len();
                        let _ = log_event(
                            log_file,
                            "info",
                            "discovery.language_pack_submitted",
                            json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype,
                                "entries": count
                            }),
                        );
                        rel_completed += count;
                        rel_processed += count;
                    }
                    Err(err) => {
                        adaptive.on_error_message(&format!("{:#}", err));
                        let _ = log_event(
                            log_file,
                            "warning",
                            "discovery.language_pack_callback_failed",
                            json!({
                                "relation_id": relation.id,
                                "business_line": business_line,
                                "subtype": subtype,
                                "entries": translated_entries.len(),
                                "error": snippet(&format!("{:#}", err))
                            }),
                        );
                        rel_processed += translated_entries.len();
                        rel_failed += translated_entries.len();
                    }
                }
            }
        }

        // Touch last_run_at and finalize job status
        if let Some(ref db_arc) = db {
            let conn = db_arc.lock().await;
            touch_last_run_at(&conn, &domain_key, relation.id);
        }
        // Finalize job in DB (after normal content + language-pack branches)
        if let (Some(ref db_arc), Some(jid)) = (&db, *job_id) {
            let rc = rel_completed;
            let rf = rel_failed;
            let rp = rel_processed;
            let conn = db_arc.lock().await;
            let _ =
                crate::db::jobs::increment_job_counters(&conn, jid, rc as i64, rf as i64, rp as i64);
            let has_pending = rp > (rc + rf);
            let final_status = if has_pending {
                "partial"
            } else if rf == 0 {
                "completed"
            } else if rc == 0 {
                "failed"
            } else {
                "partial"
            };
            let _ = crate::db::jobs::update_job_status(&conn, jid, final_status);
        }

        let relation_elapsed_ms = relation_started_at.elapsed().as_millis() as u64;
        let _ = log_event(
            log_file,
            "info",
            "discovery.relation_done",
            json!({
                "relation_id": relation.id,
                "processed": rel_processed,
                "completed": rel_completed,
                "failed": rel_failed,
                "elapsed_ms": relation_elapsed_ms,
            }),
        );

        (rel_processed, rel_completed, rel_failed)
        }); // end relation_joins.spawn(async move { ... })
    } // end for relation in relations

    // Collect relation concurrency results
    while let Some(join_result) = relation_joins.join_next().await {
        match join_result {
            Ok((p, c, f)) => {
                report.processed += p;
                report.completed += c;
                report.failed += f;
            }
            Err(e) => {
                let _ = log_event(
                    log_file,
                    "warning",
                    "discovery.relation_task_panicked",
                    json!({ "error": snippet(&format!("{:?}", e)) }),
                );
                report.failed += 1;
            }
        }
    }

    report.avg_elapsed_ms = domain_started_at.elapsed().as_millis() as u64;
    let _ = log_event(
        log_file,
        "info",
        "discovery.domain_done",
        json!({
            "api_base_url": wp_base,
            "processed": report.processed,
            "completed": report.completed,
            "failed": report.failed,
            "elapsed_ms": report.avg_elapsed_ms,
            "relations_count": relations_count,
        }),
    );

    Ok(report)
}

// Translation helper functions have been moved to pipeline.rs.
// Discovery execution helpers live in execute.rs.

#[cfg(test)]
mod tests;
