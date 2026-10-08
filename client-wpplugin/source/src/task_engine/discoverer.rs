//! Content discovery flow: fetch site-relations, rules, and untranslated content
//! from a WP site, translate via component, and submit results via callback.
//!
//! This is an alternative to the pre-v1.2.0 task-pull flow (puller.rs,
//! removed as dead code 2026-09-12: it was no longer in the module tree).
//! Enabled when `WorkerConfig::discovery_mode` is `true`.

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
    ensure_discovery_tasks, get_discovery_task_params, release_in_progress, touch_last_run_at,
    try_claim_in_progress, DiscoveryTaskParams,
};
use crate::db::jobs::list_resumable_items_for_client_base;
use crate::db::pending_callbacks::{
    add_pending_callback, find_pending_callback, increment_retry_pending_callback,
    remove_pending_callback, PendingCallbackEntry,
};
use crate::logging::{log_event, snippet, unix_ts};
use crate::persistence::{PendingCallback, PendingCallbackStore};
use crate::task_engine::pipeline::{
    build_translated_path, sanitize_domain_key as pipeline_sanitize_domain_key,
    sync_i18n_item_to_wp, sync_item_to_wp, translate_item_fields_with_trace_using_proxy,
};
use crate::task_engine::submitter::{
    retry_with_backoff, send_i18n_translation_callback, send_translation_callback,
};
use crate::types::*;

mod adaptive;
mod execute;
mod fetch;
mod language_pack;
mod receipts;
mod task_scope;
use self::adaptive::*;
use self::execute::*;
use self::fetch::*;
use self::language_pack::*;
use self::task_scope::*;

pub(crate) fn try_reserve_discovery_item(counter: &AtomicUsize, limit: usize) -> bool {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
            if limit == 0 || used < limit {
                used.checked_add(1)
            } else {
                None
            }
        })
        .is_ok()
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
        &worker_config.device_id,
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
    // Runtime DB for discovery params and resume (None = isolated legacy callers).
    db: Option<Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
    // Identity Contract v1.1 §5 (C-1): the binding's verified identity. This
    // discoverer serves the wpmmcc-ats task lanes; any other identity must
    // never dispatch through it (defense in depth behind the worker gate).
    binding_identity: crate::types::PluginIdentity,
) -> anyhow::Result<DomainRunReport> {
    // Lane entry fail-closed check (contract §5 row 1): refuse to run ats
    // lanes for a binding that is not wpmmcc_ats. The worker-level gate
    // normally filters earlier; this guard keeps the lane entry safe even
    // if a future caller bypasses it. No credential fields in the event.
    // FL-3 (Wave-2): the warn is emitted ONCE per (lane, domain, identity)
    // per process — a standing mismatch used to re-warn on every run-once
    // iteration (180-iteration identity_mismatch storms); repeats now skip
    // silently, still fail-closed.
    if binding_identity != crate::types::PluginIdentity::WpmmccAts {
        if crate::bindings::identity_exclusion_first_notice(
            "wpmmcc_ats_lane_entry",
            wp_base,
            binding_identity.as_wire_str(),
        ) {
            let _ = log_event(
                log_file,
                "warning",
                crate::bindings::CODE_IDENTITY_MISMATCH,
                serde_json::json!({
                    "lane": "wpmmcc_ats",
                    "binding_identity": binding_identity.as_wire_str(),
                }),
            );
        }
        return Ok(DomainRunReport {
            api_base_url: wp_base.to_string(),
            ..DomainRunReport::default()
        });
    }
    // FL-7 dead-domain circuit: a site whose lane entry point failed at the
    // transport level three times in a row is skipped for the cooldown —
    // run-once budget then flows to live domains instead of burning one
    // full failed scan per iteration on a dead one (observed: dead-domain
    // relations starved fresh relations of the 180-iteration budget).
    let domain_circuit = crate::task_engine::backoff::check_domain(wp_base);
    if domain_circuit.blocked {
        let _ = log_event(
            log_file,
            "info",
            "discovery.domain_circuit_open",
            json!({
                "api_base_url": wp_base,
                "remaining_secs": domain_circuit.remaining_secs,
                "consecutive_failures": domain_circuit.consecutive_failures,
            }),
        );
        return Ok(DomainRunReport {
            api_base_url: wp_base.to_string(),
            ..DomainRunReport::default()
        });
    }
    let domain_started_at = std::time::Instant::now();
    // GAP-06 收尾: run 级 trace。run 开始即生成（disc- 前缀，与 sync 泳道的
    // sync- 前缀同族），在本 run 的全部出站请求上以 X-WPTSALL-Trace-Id 头携带，
    // 并随 run 报告/日志回传。上方早退分支未发出站请求，不生成 trace。
    let trace_id = format!("disc-{}", crate::auth::build_request_id("run"));
    let _run_trace_guard = crate::auth::scoped_run_trace_id(trace_id.clone());
    // Isolated legacy callers without a DB retain the domain-level outbox
    // drain. Runtime callers with a DB drain after /site-relations and only
    // for enabled relation-scoped tasks.
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
    // Default per-run budget guards real-time starvation (L3 capacity finding):
    // with an unbounded run, one discover_and_translate call can chew a large
    // historical backlog for many minutes, and fresh outbox lifecycle events
    // (e.g. post updates) are only drained at the START of a cycle — they wait
    // unclaimed until the whole backlog is done. A bounded default keeps cycles
    // short so the outbox is re-drained every cycle within minutes; the worker
    // tick loop starts the next cycle after poll_seconds, so overall backlog
    // throughput is unchanged. Set the env to 0 to restore unbounded runs.
    let max_items_per_run = worker_config.discovery_max_items_per_run;

    // Extract domain name for DB keying (use wp_base as the key)
    let domain_key = wp_base.to_string();

    // 1. Fetch site relations
    let relations_url = format!("{}/site-relations", wp_base);
    adaptive.wait_turn().await;
    let relations_resp = match wp_get_json_with_transport_and_secret::<RelationsResponse>(
        client,
        &relations_url,
        token,
        &worker_config.worker_id,
        &worker_config.device_id,
        route_secret,
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => {
            // FL-7: count transport-level entry failures so a dead domain
            // trips the whole-domain circuit (skipped for the cooldown); a
            // PERMANENT status on the entry route opens the scan circuit
            // instead (the outbox lane keeps flowing). The error itself
            // still propagates — this iteration's domain run fails loudly
            // exactly as before.
            let err_text = format!("{:#}", err);
            let permanent =
                crate::task_engine::backoff::is_permanent_http_failure_message(&err_text);
            let decision = if permanent {
                crate::task_engine::backoff::record_domain_scan_permanent_failure(wp_base)
            } else {
                crate::task_engine::backoff::record_domain_transport_failure(wp_base)
            };
            let _ = log_event(
                log_file,
                "info",
                "discovery.site_relations_circuit_recorded",
                json!({
                    "api_base_url": wp_base,
                    "consecutive_failures": decision.consecutive_failures,
                    "circuit_open": decision.circuit_open,
                    "permanent": permanent,
                }),
            );
            return Err(err.context("Failed to fetch site relations"));
        }
    };
    // FL-7: a live lane entry resets the dead-domain streak.
    crate::task_engine::backoff::record_domain_success(wp_base);
    adaptive.on_success();

    let mut relations = relations_resp.relations;
    if let Some(ref db_arc) = db {
        let mut relation_params_by_id: HashMap<i64, DiscoveryTaskParams> = HashMap::new();
        for relation in &relations {
            let params = {
                let conn = db_arc.lock().await;
                ensure_discovery_tasks(&conn, &domain_key, &[relation.id])?;
                get_discovery_task_params(&conn, &domain_key, relation.id)?
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
        trace_id: trace_id.clone(),
        ..DomainRunReport::default()
    };

    if relations.is_empty() {
        // Do not strand a newly leased change merely because a relation was
        // removed between claim and discovery. Its WP lease will expire and it
        // will be retried after the relation is restored or cleaned up.
        return Ok(report);
    }

    if db.is_some() {
        // 批 D ② 复审裁定（批 M，2026-09-23）：本块是**关系级定向 drain**
        // （逐启用关系、带 relation_id 租约），相比批 D 时点的域级单次全局
        // drain 已是形态升级；20s 迭代节奏（WPTSALL_POLL_SECONDS）是产品
        // 设计的变更拉取节奏。原挂账的两种剩余形态裁定如下：
        //   - 「处理中穿插」（relation 7 翻译期间的新 change 等 relation 8
        //     下一轮）：需把本前置环 + outbox 执行管线整体搬进主关系环
        //     （:1123 巨型环）——真重构，且 WP 侧 outbox 行本有
        //     available_at 退避、无紧急性，收益/风险比不成立，不做；
        //   - 「事件驱动/独立协程」：需 WP 侧推送通知端点（新产品面），
        //     冻结待该端点立项后一并设计。
        // 本批（批 M）据此把批 D ② 从活跃挂账转冻结。
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
    //
    // One job per relation is created lazily so every outbox-executed item
    // gets a job-item row: the jobs dashboard and the pending-review pool
    // are both driven by those rows, and without them review-held outbox
    // items were invisible and could never be approved.
    let mut outbox_job_ids: std::collections::HashMap<i64, Option<i64>> =
        std::collections::HashMap::new();
    let mut outbox_failed_relations = std::collections::HashSet::new();
    for change in leased_outbox_changes {
        // FL-2b re-offer hold: a row this process already executed and acked
        // (retry-with-backoff or completed) must not execute again while its
        // hold lasts. A site that immediately re-offers an acked row (or
        // ignores `available_at`) would otherwise burn the whole run-once
        // iteration budget on the same row (observed: 180/180 iterations,
        // zero terminal state). Keyed by (domain, outbox_id) — ids are
        // per-site and collide across sites.
        let hold_remaining =
            crate::task_engine::backoff::outbox_hold_remaining(wp_base, change.outbox_id);
        if hold_remaining > 0 {
            report.pulled += 1;
            let _ = log_event(
                log_file,
                "info",
                "discovery.outbox_reoffer_held",
                json!({
                    "api_base_url": wp_base,
                    "outbox_id": change.outbox_id,
                    "hold_remaining_secs": hold_remaining,
                }),
            );
            continue;
        }
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
        let outbox_job_id = match outbox_job_ids.entry(relation.id) {
            std::collections::hash_map::Entry::Vacant(vacant) => {
                let jid = if let Some(ref db_arc) = db {
                    let conn = db_arc.lock().await;
                    crate::db::jobs::create_job(
                        &conn,
                        &crate::db::jobs::CreateJobRequest {
                            domain: domain_key.clone(),
                            relation_id: relation.id,
                            business_line: "discovery".to_string(),
                            triggered_by: "outbox".to_string(),
                        },
                    )
                    .ok()
                    .map(|id| {
                        let _ = crate::db::jobs::update_job_status(&conn, id, "running");
                        id
                    })
                } else {
                    None
                };
                if let Some(id) = jid {
                    let _ = log_event(
                        log_file,
                        "info",
                        "job.created",
                        json!({
                            "job_id": id,
                            "relation_id": relation.id,
                            "triggered_by": "outbox",
                        }),
                    );
                }
                *vacant.insert(jid)
            }
            std::collections::hash_map::Entry::Occupied(occupied) => *occupied.get(),
        };
        let rules_url = format!("{}/rules?relation_id={}", wp_base, relation.id);
        let rules = match wp_get_json_with_transport_and_secret::<RulesResponse>(
            client,
            &rules_url,
            token,
            &worker_config.worker_id,
            &worker_config.device_id,
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
            get_discovery_task_params(&conn, &domain_key, relation.id)?
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
        // Relation poison (structural): the relation's post/term lane tripped
        // the structural poison. Skip WITHOUT acking — the event stays
        // pending, is re-listed cheaply each cycle, and gets picked up as
        // soon as a configuration change clears the poison.
        if crate::task_engine::backoff::check_relation(wp_base, relation.id).blocked {
            report.pulled += 1;
            let _ = log_event(
                log_file,
                "info",
                "discovery.outbox_relation_structural_skip",
                json!({
                    "outbox_id": change.outbox_id,
                    "relation_id": relation.id,
                }),
            );
            continue;
        }
        // P8 client-side backoff: never re-execute an identity whose recent
        // attempts failed until the cooldown expires. The identity key
        // covers BOTH hot loops (outbox fast path + server retry queue).
        // The provider is not contacted for a blocked row; it is acked back
        // to pending with a backoff hint (the WP side honors it via
        // available_at).
        let backoff = crate::task_engine::backoff::check(
            wp_base,
            relation.id,
            outbox_item.object_type.as_str(),
            outbox_item.object_id,
        );
        if backoff.blocked {
            report.pulled += 1;
            report.retried += 1;
            let _ = log_event(
                log_file,
                "warning",
                "discovery.outbox_backoff_skip",
                json!({
                    "outbox_id": change.outbox_id,
                    "remaining_secs": backoff.remaining_secs,
                    "consecutive_failures": backoff.consecutive_failures,
                    "circuit_open": backoff.circuit_open,
                }),
            );
            let ack_url = format!("{}/content-changes/{}/ack", wp_base, change.outbox_id);
            let ack_body = json!({
                "outcome": "retry",
                "error": format!(
                    "client backoff: execution suppressed for {}s after {} consecutive failures{}",
                    backoff.remaining_secs,
                    backoff.consecutive_failures,
                    if backoff.circuit_open { " (circuit open)" } else { "" }
                ),
                "backoff_seconds": backoff.remaining_secs,
            });
            if let Err(ack_err) = wp_request_with_transport(
                client,
                reqwest::Method::POST,
                &ack_url,
                token,
                &worker_config.worker_id,
                &worker_config.device_id,
                &ack_body,
                route_secret,
            )
            .await
            {
                let _ = log_event(
                    log_file,
                    "warning",
                    "discovery.outbox_backoff_ack_failed",
                    json!({
                        "outbox_id": change.outbox_id,
                        "error": snippet(&format!("{:#}", ack_err)),
                    }),
                );
            }
            // FL-2b: hold the row for the hinted backoff so a site that
            // re-offers it immediately cannot re-burn iterations on it.
            crate::task_engine::backoff::note_outbox_hold(
                wp_base,
                change.outbox_id,
                backoff.remaining_secs.max(30),
            );
            continue;
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
            outbox_job_id,
            &task_params,
            None,
        )
        .await;
        report.pulled += 1;
        report.processed += 1;
        match result {
            Ok((
                ref outcome @ TranslateContentOutcome::Completed,
                fields_count,
                ref execution_summary,
            ))
            | Ok((
                ref outcome @ TranslateContentOutcome::PendingCallback,
                fields_count,
                ref execution_summary,
            )) => {
                report.completed += 1;
                crate::task_engine::backoff::record_success(
                    wp_base,
                    relation.id,
                    outbox_item.object_type.as_str(),
                    outbox_item.object_id,
                );
                crate::task_engine::backoff::record_relation_success(wp_base, relation.id);
                let _ = log_event(
                    log_file,
                    "info",
                    "discovery.outbox_executed",
                    json!({
                        "outbox_id": change.outbox_id, "task_id": change.task_id,
                        "client_task_id": change.client_task_id,
                    }),
                );
                // FL-12 (SIM-15): the outbox lane never wrote the
                // materialized success record, so the scan lane's
                // has_materialized_success_record dedup missed
                // outbox-completed objects — the run-once loop's later
                // scan pass re-translated and re-callbacked every one of
                // them (double provider cost, duplicate site writes under
                // lane-divergent idempotency keys). Record the success here
                // so the scan pass skips instead of re-billing.
                if *outcome == TranslateContentOutcome::Completed {
                    if let Some(ref db_arc) = db {
                        let object_type_key =
                            normalize_object_type_key(outbox_item.object_type.as_str()).to_string();
                        let matched_rule = rules
                            .iter()
                            .find(|r| r.object_name == outbox_item.subtype)
                            .or_else(|| {
                                rules
                                    .iter()
                                    .find(|r| r.object_name == outbox_item.object_type)
                            });
                        let derived_business_line =
                            crate::task_engine::pipeline::derive_business_line_from_rule(
                                matched_rule,
                                outbox_item.object_type.as_str(),
                            )
                            .to_string();
                        let conn = db_arc.lock().await;
                        let _ = crate::db::translations::insert_translation_record(
                            &conn,
                            &crate::db::translations::InsertTranslationRecord {
                                domain: wp_base.to_string(),
                                relation_id: Some(relation.id),
                                object_id: Some(outbox_item.object_id),
                                object_type: Some(object_type_key),
                                business_line: Some(derived_business_line),
                                source_lang: relation.source_lang.clone(),
                                target_lang: relation.target_lang.clone(),
                                status: "success".to_string(),
                                execution_ms: None,
                                worker_id: Some(worker_config.worker_id.clone()),
                                idempotency_key: None,
                                fields_count: fields_count as i32,
                                error_message: None,
                                component_ids: execution_summary.component_ids.clone(),
                                media_mappings_count: execution_summary.media_mappings_count,
                                failed_fields_count: execution_summary.failed_fields_count,
                                primary_failure_reason: execution_summary
                                    .primary_failure_reason
                                    .clone(),
                            },
                        );
                    }
                }
                // FL-2b: the callback is the terminal signal for this row; a
                // site that re-offers it within the settle window is skipped
                // instead of re-translating (duplicate callback spam).
                crate::task_engine::backoff::note_outbox_hold(
                    wp_base,
                    change.outbox_id,
                    crate::task_engine::backoff::OUTBOX_COMPLETED_HOLD_SECS,
                );
            }
            Ok((TranslateContentOutcome::NoChanges, _, _))
            | Ok((TranslateContentOutcome::PendingReview, _, _)) => {
                // A no-op is terminal for this exact snapshot. Explicitly ack
                // it so an empty/untranslatable attachment or field does not
                // hold a lease until timeout.
                crate::task_engine::backoff::record_success(
                    wp_base,
                    relation.id,
                    outbox_item.object_type.as_str(),
                    outbox_item.object_id,
                );
                crate::task_engine::backoff::record_relation_success(wp_base, relation.id);
                let ack_url = format!("{}/content-changes/{}/ack", wp_base, change.outbox_id);
                match wp_request_with_transport(
                    client,
                    reqwest::Method::POST,
                    &ack_url,
                    token,
                    &worker_config.worker_id,
                    &worker_config.device_id,
                    &json!({ "outcome": "completed" }),
                    route_secret,
                )
                .await
                {
                    Ok(_) => {
                        report.completed += 1;
                        // FL-2b: completed-ack settle window — a site that
                        // re-offers the row immediately is skipped.
                        crate::task_engine::backoff::note_outbox_hold(
                            wp_base,
                            change.outbox_id,
                            crate::task_engine::backoff::OUTBOX_COMPLETED_HOLD_SECS,
                        );
                    }
                    Err(_) => {
                        report.retried += 1;
                        crate::task_engine::backoff::note_outbox_hold(
                            wp_base,
                            change.outbox_id,
                            30,
                        );
                    }
                }
            }
            Err(err) => {
                report.failed += 1;
                outbox_failed_relations.insert(relation.id);
                let error_snippet = snippet(&format!("{:#}", err));
                // P8: record the failure and arm the cooldown before acking,
                // so the ack can carry the backoff hint for observability.
                // Structural failures (no component runtime for the content's
                // formats) arm the permanent tier instead: the outbox row
                // returns to pending, but the client-side claim filter stops
                // re-offering it until the binding changes.
                let backoff = if crate::task_engine::backoff::is_structural_failure_message(
                    &format!("{:#}", err),
                ) {
                    let decision = crate::task_engine::backoff::record_structural_failure(
                        wp_base,
                        relation.id,
                        outbox_item.object_type.as_str(),
                        outbox_item.object_id,
                    );
                    crate::task_engine::backoff::record_structural_relation_failure(
                        wp_base,
                        relation.id,
                    );
                    decision
                } else {
                    crate::task_engine::backoff::record_failure(
                        wp_base,
                        relation.id,
                        outbox_item.object_type.as_str(),
                        outbox_item.object_id,
                    )
                };
                if backoff.circuit_open {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "discovery.outbox_circuit_open",
                        json!({
                            "outbox_id": change.outbox_id,
                            "consecutive_failures": backoff.consecutive_failures,
                            "cooldown_secs": backoff.remaining_secs,
                        }),
                    );
                }
                let _ = log_event(
                    log_file,
                    "warning",
                    "discovery.outbox_execution_failed",
                    json!({
                        "outbox_id": change.outbox_id, "task_id": change.task_id,
                        "error": error_snippet,
                        "backoff_secs": backoff.remaining_secs,
                    }),
                );
                // Write the failure back to WP so the outbox row returns to
                // pending with last_error recorded (retry outcome) instead of
                // holding a dead lease until timeout. Best-effort: an ack
                // failure only means the lease-timeout path stays as fallback.
                let ack_url = format!("{}/content-changes/{}/ack", wp_base, change.outbox_id);
                let ack_body = json!({
                    "outcome": "retry",
                    "error": error_snippet,
                    "backoff_seconds": backoff.remaining_secs,
                });
                if let Err(ack_err) = wp_request_with_transport(
                    client,
                    reqwest::Method::POST,
                    &ack_url,
                    token,
                    &worker_config.worker_id,
                    &worker_config.device_id,
                    &ack_body,
                    route_secret,
                )
                .await
                {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "discovery.outbox_fail_ack_failed",
                        json!({
                            "outbox_id": change.outbox_id,
                            "error": snippet(&format!("{:#}", ack_err)),
                        }),
                    );
                }
                // FL-2b: hold the row for the hinted backoff so a site that
                // re-offers it immediately cannot re-burn iterations on it.
                crate::task_engine::backoff::note_outbox_hold(
                    wp_base,
                    change.outbox_id,
                    backoff.remaining_secs.max(30),
                );
            }
        }
    }

    // Finalize the outbox jobs from their item rows (the same signal the
    // dashboard reads): pending items hold the job open, all-failed marks
    // it failed, otherwise it completed.
    if let Some(ref db_arc) = db {
        let conn = db_arc.lock().await;
        for (relation_id, jid) in outbox_job_ids.iter() {
            let Some(jid) = *jid else { continue };
            let empty_status = if outbox_failed_relations.contains(relation_id) {
                "failed"
            } else {
                "completed"
            };
            let final_status =
                match crate::db::jobs::project_job_from_items(&conn, jid, true, empty_status) {
                    Ok(status) => status,
                    Err(err) => {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "job.finalize_failed",
                            json!({
                                "job_id": jid, "error": snippet(&format!("{err:#}")),
                            }),
                        );
                        return Err(err.context("finalize outbox job projection"));
                    }
                };
            let progress = crate::db::jobs::get_job_progress(&conn, jid)?;
            let _ = log_event(
                log_file,
                "info",
                "discovery.outbox_job_finalized",
                json!({
                    "relation_id": relation_id,
                    "job_id": jid,
                    "status": final_status,
                    "items": progress.total,
                }),
            );
        }
    }

    // The runtime lease owner recovered abandoned claims at boot. A normal
    // scan must not age-delete claims that can still belong to live work.
    if let Some(ref db_arc) = db {
        let conn = db_arc.lock().await;
        crate::db::discovery_tasks::in_progress_inventory(&conn)?;
    }

    // Starting a scan is not authorization to delete saved results or history.
    // Retention defaults to manual cleanup, including completed work.

    // Resume previously interrupted items (crash recovery).
    // Boot has already reset unfinished translation; saved results only need
    // sync. A filename slug is not a database origin (it omits the WP path).
    if let Some(ref db_arc) = db {
        let pipeline_domain_key = pipeline_sanitize_domain_key(wp_base);
        let resumable = {
            let conn = db_arc.lock().await;
            list_resumable_items_for_client_base(
                &conn,
                wp_base,
                &pipeline_domain_key,
                &["translated"],
            )?
            .into_iter()
            .filter(|item| {
                relations
                    .iter()
                    .any(|relation| relation.id == item.relation_id)
            })
            .collect::<Vec<_>>()
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
                            let has_entries =
                                crate::task_engine::pipeline::translated_payload_has_i18n_entries(
                                    &item.translated_path,
                                );
                            sync_i18n_item_to_wp(
                                db_arc,
                                client,
                                item.id,
                                &item.translated_path,
                                wp_base,
                                token,
                                worker_config,
                                log_file,
                                &callback_sem,
                            )
                            .await
                            .map(|count| {
                                if has_entries {
                                    count
                                } else {
                                    count.max(1)
                                }
                            })
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
    let discovery_runtime_lease = if let Some(ref db_arc) = db {
        let conn = db_arc.lock().await;
        crate::db::runtime::RuntimeLease::for_connection(&conn)
    } else {
        None
    };
    let mut relation_joins: JoinSet<anyhow::Result<(usize, usize, usize)>> = JoinSet::new();

    let relations_count = relations.len();

    // FL-2b scan circuit: a domain whose claim/content endpoints answered
    // with a permanent HTTP status (404/410 — route missing, e.g. an ATS
    // plugin older than the claim contract) stops burning scan pages on it
    // for the cooldown. The outbox fast path above is a DIFFERENT endpoint
    // family and stays active — a claim-route gap must not stop lifecycle
    // events from flowing. (Shadowing the relation list keeps the loop a
    // zero-iteration pass instead of re-indenting the whole scan body.)
    let scan_circuit = crate::task_engine::backoff::check_domain_scan(wp_base);
    let relations = if scan_circuit.blocked {
        let _ = log_event(
            log_file,
            "info",
            "discovery.scan_circuit_open",
            json!({
                "api_base_url": wp_base,
                "remaining_secs": scan_circuit.remaining_secs,
                "consecutive_failures": scan_circuit.consecutive_failures,
            }),
        );
        Vec::new()
    } else {
        relations
    };

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
        let relation_lease = discovery_runtime_lease.clone();

        relation_joins.spawn(async move {
        let runtime_lease = relation_lease;
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
            ensure_discovery_tasks(&conn, &domain_key, &[relation.id])?;
            get_discovery_task_params(&conn, &domain_key, relation.id)?
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
            return Ok((rel_processed, rel_completed, rel_failed));
        }

        let rules_url = format!("{}/rules?relation_id={}", wp_base, relation.id);
        adaptive.wait_turn().await;
        let rules = match wp_get_json_with_transport_and_secret::<RulesResponse>(
            client,
            &rules_url,
            token,
            &worker_config.worker_id, &worker_config.device_id,
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
                return Ok((rel_processed, rel_completed, rel_failed));
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
            let callbacks = match crate::db::pending_callbacks::count_pending_for_relation(&conn, &domain_key, relation.id) {
                Ok(count) => count,
                Err(error) => {
                    let _ = log_event(log_file, "error", "discovery.pending_callback_read_failed",
                        json!({ "relation_id": relation.id, "error": error.to_string() }));
                    return Ok((rel_processed, rel_completed, rel_failed + 1));
                }
            };
            callbacks
                + crate::db::jobs::count_items_by_status_for_relation(&conn, wp_base, relation.id, "translated")?
                + crate::db::jobs::count_legacy_items_by_status_for_relation(&conn, wp_base, &pipeline_domain_key, relation.id, "translated")?
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
                return Ok((rel_processed, rel_completed, rel_failed));
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
        if let Some(id) = job_id {
            let _ = log_event(
                log_file,
                "info",
                "job.created",
                json!({
                    "job_id": id,
                    "relation_id": relation.id,
                    "triggered_by": "auto",
                }),
            );
        }
        let job_id = Arc::new(job_id);

        // 2b. Process retry queue entries before normal discovery.
        //
        // Pending retries are fetched from the retry_queue table and translated
        // using the same pipeline. We fetch their content via `include_ids`
        // parameter so WP returns only the specific objects.
        if let Some(ref db_arc) = db {
            let retries = {
                let conn = db_arc.lock().await;
                crate::db::translations::get_pending_retries(&conn, &domain_key, relation.id)?
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
                        &worker_config.worker_id, &worker_config.device_id,
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
                                // P8: the retry queue re-offers content every
                                // cycle; gate it on the same identity-keyed
                                // backoff as the outbox fast path so a bad
                                // credential cannot keep hammering the
                                // provider through this second loop.
                                let backoff = crate::task_engine::backoff::check(
                                    wp_base,
                                    relation.id,
                                    object_type.as_str(),
                                    oid,
                                );
                                if backoff.blocked {
                                    let _ = log_event(
                                        log_file,
                                        "warning",
                                        "discovery.retry_item_backoff_skip",
                                        json!({
                                            "relation_id": relation.id,
                                            "object_id": oid,
                                            "object_type": object_type.as_str(),
                                            "remaining_secs": backoff.remaining_secs,
                                            "consecutive_failures": backoff.consecutive_failures,
                                            "circuit_open": backoff.circuit_open,
                                        }),
                                    );
                                    continue;
                                }
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
                                    // Retry items join the relation's job so
                                    // they appear in the dashboard/review pool.
                                    *job_id,
                                    &params,
                                    None,
                                )
                                .await;
                                rel_processed += 1;
                                match result {
                                    Ok((outcome, fields_count, summary)) => {
                                        crate::task_engine::backoff::record_success(
                                            wp_base,
                                            relation.id,
                                            object_type.as_str(),
                                            oid,
                                        );
                                        crate::task_engine::backoff::record_relation_success(
                                            wp_base,
                                            relation.id,
                                        );
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
                                            {
                                                let conn = db_arc.lock().await;
                                                crate::db::translations::mark_retry_done(
                                                    &conn,
                                                    &domain_key,
                                                    relation.id,
                                                    item.object_type.as_str(),
                                                    oid,
                                                ).context("retain original retry after unconfirmed completion")?;
                                            }
                                            rel_completed += 1;
                                        }
                                    }
                                    Err(err) => {
                                        rel_failed += 1;
                                        let err_str = format!("{:#}", err);
                                        // Structural failures (no component
                                        // runtime can ever serve this content
                                        // until the binding changes) go to the
                                        // permanent tier.
                                        let backoff = if crate::task_engine::backoff::is_structural_failure_message(&err_str) {
                                            let decision = crate::task_engine::backoff::record_structural_failure(
                                                wp_base,
                                                relation.id,
                                                object_type.as_str(),
                                                oid,
                                            );
                                            crate::task_engine::backoff::record_structural_relation_failure(
                                                wp_base, relation.id,
                                            );
                                            decision
                                        } else {
                                            crate::task_engine::backoff::record_failure(
                                                wp_base,
                                                relation.id,
                                                object_type.as_str(),
                                                oid,
                                            )
                                        };
                                        if backoff.circuit_open {
                                            let _ = log_event(
                                                log_file,
                                                "warning",
                                                "discovery.retry_item_circuit_open",
                                                json!({
                                                    "relation_id": relation.id,
                                                    "object_id": oid,
                                                    "object_type": object_type.as_str(),
                                                    "consecutive_failures": backoff.consecutive_failures,
                                                    "cooldown_secs": backoff.remaining_secs,
                                                }),
                                            );
                                        }
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
            // Relation poison (structural): a fully-doomed relation — every
            // attempted post/term item failed structurally with no success in
            // between — skips its whole post/term discovery+claim lane (no
            // page fetches, no claim leases, no translate attempts) until a
            // configuration change clears the poison. Mixed relations never
            // trip it: any success resets the streak. Other lanes (language
            // packs, outbox re-sync) are unaffected.
            if crate::task_engine::backoff::check_relation(wp_base, relation.id).blocked {
                let _ = log_event(
                    log_file,
                    "info",
                    "discovery.relation_structural_skip",
                    json!({
                        "relation_id": relation.id,
                        "data_type": content_data_type,
                    }),
                );
                continue;
            }
            // batch_parallel pipelines share an AtomicI64 page counter so they each
            // fetch different pages for the current data_type.
            let page_counter = Arc::new(AtomicI64::new(1));
            let total_pages = Arc::new(AtomicI64::new(i64::MAX));

            let rules_arc = Arc::new(rules.clone());
            let mut batch_joins: JoinSet<anyhow::Result<(usize, usize, usize)>> = JoinSet::new();

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
                let batch_lease = runtime_lease.clone();

                batch_joins.spawn(async move {
                    let runtime_lease = batch_lease;
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
                            &worker_config.worker_id, &worker_config.device_id,
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
                        // Claim storage exists for post/taxonomy/option lanes
                        // (option lease: relation-scoped Option_Sync_State_Service,
                        // STA-02 2026-09-22 — the WP callback for options rejects
                        // unclaimed items with claim_required, and dual clients
                        // previously raced with no lease at all).
                        let mut content_items = content.items;
                        if let Some(claim_shape) = content_claim_shape(&content_data_type) {
                            let is_term_claim = matches!(claim_shape, ContentClaimShape::Term);
                            // Claim-filter: backoff-blocked items (transient
                            // cooldown or structural skip) must not even be
                            // LEASED. The claim POST itself is WP-side write
                            // load, and pre-fix every cycle re-leased the whole
                            // page — including every item the translate gate
                            // would have skipped — so structurally-doomed
                            // relations kept hammering the WP claim endpoint
                            // with zero useful work.
                            let before_claim_filter = content_items.len();
                            content_items.retain(|candidate| {
                                !crate::task_engine::backoff::check(
                                    &wp_base,
                                    relation.id,
                                    candidate.object_type.as_str(),
                                    candidate.object_id,
                                )
                                .blocked
                            });
                            if content_items.len() < before_claim_filter {
                                let _ = log_event(
                                    &log_file,
                                    "info",
                                    "discovery.content_claim_backoff_filtered",
                                    json!({
                                        "relation_id": relation.id,
                                        "data_type": content_data_type,
                                        "before": before_claim_filter,
                                        "after": content_items.len(),
                                    }),
                                );
                            }
                            if content_items.is_empty() {
                                // Whole page is blocked: no claim POST, no
                                // translate. Later pages may hold fresh items.
                                continue;
                            }
                            let claim_items = build_content_claim_items(&content_items, claim_shape);
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
                                &worker_config.worker_id, &worker_config.device_id,
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
                                    // FL-2b: a PERMANENT claim-route status (404/410 —
                                    // route never deployed, or a plugin older than
                                    // the claim contract) opens the domain scan
                                    // circuit so the run stops re-attempting it
                                    // every iteration; the outbox lane keeps
                                    // flowing (different endpoint family).
                                    if crate::task_engine::backoff::is_permanent_http_failure_message(&err_str) {
                                        let decision = crate::task_engine::backoff::record_domain_scan_permanent_failure(wp_base.as_str());
                                        let _ = log_event(
                                            &log_file,
                                            "info",
                                            "discovery.scan_circuit_open",
                                            json!({
                                                "api_base_url": wp_base,
                                                "reason": "claim_route_permanent_status",
                                                "remaining_secs": decision.remaining_secs,
                                            }),
                                        );
                                    }
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
                            if !try_reserve_discovery_item(&items_processed_total, max_items_per_run) {
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

                            // P8: gate regular content discovery on the same
                            // identity-keyed backoff as the outbox/retry loops.
                            // An all-fields-failed object would otherwise be
                            // re-discovered and re-translated every cycle.
                            let backoff = crate::task_engine::backoff::check(
                                &wp_base,
                                relation.id,
                                item.object_type.as_str(),
                                item.object_id,
                            );
                            if backoff.blocked {
                                let _ = log_event(
                                    &log_file,
                                    "warning",
                                    "discovery.content_item_backoff_skip",
                                    json!({
                                        "relation_id": relation.id,
                                        "data_type": content_data_type,
                                        "object_id": item.object_id,
                                        "object_type": item.object_type.as_str(),
                                        "remaining_secs": backoff.remaining_secs,
                                        "consecutive_failures": backoff.consecutive_failures,
                                        "circuit_open": backoff.circuit_open,
                                    }),
                                );
                                items_processed_total.fetch_sub(1, Ordering::Relaxed);
                                continue;
                            }

                            // In-progress dedup: skip if another pipeline is already translating this item.
                            // An authority failure is not evidence of another owner.
                            let claimed_local = if let Some(ref db_arc) = db {
                                let conn = db_arc.lock().await;
                                try_claim_in_progress(
                                    &conn,
                                    &domain_key,
                                    relation.id,
                                    item.object_type.as_str(),
                                    item.object_id,
                                ).context("read and confirm retained discovery claim")?
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
                                        ).context("confirm discovery claim release after cancelled admission")?;
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
                            let item_lease = runtime_lease.clone();
                            item_joins.spawn(async move {
                                let _lease = item_lease;
                                let _permit = permit;
                                // P8: keep the identity for backoff accounting
                                // (translate_content_item_owned consumes wp_base/log_file).
                                let wp_base_for_backoff = wp_base.clone();
                                let log_file_for_backoff = log_file.clone();
                                let mut result = translate_content_item_owned(
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
                                    if let Err(error) = release_in_progress(
                                        &conn,
                                        &domain_key,
                                        relation_id_for_release,
                                        object_type.as_str(),
                                        object_id,
                                    ) {
                                        if result.is_ok() {
                                            result = Err(error.context("retain unconfirmed discovery claim release"));
                                        } else {
                                            let _ = log_event(&log_file_for_backoff, "error",
                                                "discovery.claim_release_unconfirmed",
                                                json!({"relation_id":relation_id_for_release,"object_id":object_id,
                                                    "object_type":object_type.as_str()}));
                                        }
                                    }
                                }

                                match result {
                                    Ok(true) => {
                                        crate::task_engine::backoff::record_success(
                                            &wp_base_for_backoff,
                                            relation_id_for_release,
                                            object_type.as_str(),
                                            object_id,
                                        );
                                        // Success proves the relation is not
                                        // fully doomed: reset its poison streak.
                                        crate::task_engine::backoff::record_relation_success(
                                            &wp_base_for_backoff,
                                            relation_id_for_release,
                                        );
                                        (true, false)
                                    }
                                    Ok(false) => {
                                        // No-op is terminal for this snapshot;
                                        // it clears any stale cooldown.
                                        crate::task_engine::backoff::record_success(
                                            &wp_base_for_backoff,
                                            relation_id_for_release,
                                            object_type.as_str(),
                                            object_id,
                                        );
                                        crate::task_engine::backoff::record_relation_success(
                                            &wp_base_for_backoff,
                                            relation_id_for_release,
                                        );
                                        (false, false)
                                    }
                                    Err(err) => {
                                        let err_str = format!("{:#}", err);
                                        // Structural failures (no component
                                        // runtime can ever serve this content
                                        // until the binding changes) go to the
                                        // permanent tier: one attempt per
                                        // process lifetime instead of a
                                        // cooldown-bounded re-churn.
                                        let backoff = if crate::task_engine::backoff::is_structural_failure_message(&err_str) {
                                            let decision = crate::task_engine::backoff::record_structural_failure(
                                                &wp_base_for_backoff,
                                                relation_id_for_release,
                                                object_type.as_str(),
                                                object_id,
                                            );
                                            // Feed the relation poison counter: a
                                            // fully-doomed relation stops its whole
                                            // post/term lane after the threshold.
                                            crate::task_engine::backoff::record_structural_relation_failure(
                                                &wp_base_for_backoff,
                                                relation_id_for_release,
                                            );
                                            let _ = log_event(
                                                &log_file_for_backoff,
                                                "warning",
                                                "discovery.content_item_structural_skip",
                                                json!({
                                                    "relation_id": relation_id_for_release,
                                                    "object_id": object_id,
                                                    "object_type": object_type.as_str(),
                                                    "consecutive_failures": decision.consecutive_failures,
                                                    "error": snippet(&err_str),
                                                }),
                                            );
                                            decision
                                        } else {
                                            crate::task_engine::backoff::record_failure(
                                                &wp_base_for_backoff,
                                                relation_id_for_release,
                                                object_type.as_str(),
                                                object_id,
                                            )
                                        };
                                        if backoff.circuit_open {
                                            let _ = log_event(
                                                &log_file_for_backoff,
                                                "warning",
                                                "discovery.content_item_circuit_open",
                                                json!({
                                                    "relation_id": relation_id_for_release,
                                                    "object_id": object_id,
                                                    "object_type": object_type.as_str(),
                                                    "consecutive_failures": backoff.consecutive_failures,
                                                    "cooldown_secs": backoff.remaining_secs,
                                                }),
                                            );
                                        }
                                        (false, true)
                                    }
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

                    Ok((batch_completed, batch_processed, batch_failed))
                });
            }

            // Collect batch pipeline results for this data_type
            while let Some(join_result) = batch_joins.join_next().await {
                match join_result {
                    Ok(Ok((c, p, f))) => {
                        rel_completed += c;
                        rel_processed += p;
                        rel_failed += f;
                    }
                    Ok(Err(error)) => return Err(error).context("discovery retained-state authority failed"),
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
                    &worker_config.worker_id, &worker_config.device_id,
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
                    &worker_config.worker_id, &worker_config.device_id,
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
                        // FL-2b: permanent claim-route status opens the
                        // domain scan circuit (outbox lane unaffected).
                        if crate::task_engine::backoff::is_permanent_http_failure_message(&err_str) {
                            let decision =
                                crate::task_engine::backoff::record_domain_scan_permanent_failure(wp_base);
                            let _ = log_event(
                                log_file,
                                "info",
                                "discovery.scan_circuit_open",
                                json!({
                                    "api_base_url": wp_base,
                                    "reason": "claim_route_permanent_status",
                                    "remaining_secs": decision.remaining_secs,
                                }),
                            );
                        }
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
                // FL-9 leak guard (see execute.rs): an unbound language-pack
                // task must not fall back onto another relation's component.
                let unbound_lp_fallback =
                    scoped_registry.is_none() && effective_component_id_override.trim().is_empty();
                let mut lp_claimed: Option<std::collections::HashSet<String>> = None;
                if unbound_lp_fallback {
                    if let Some(ref db_arc) = db {
                        let conn = db_arc.lock().await;
                        let claimed =
                            crate::db::discovery_tasks::selected_components_claimed_by_other_relations(
                                &conn,
                                &domain_key,
                                relation.id,
                            )?;
                        if !claimed.is_empty() {
                            lp_claimed = Some(claimed);
                        }
                    }
                }
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
                    lp_claimed.as_ref(),
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
                let component_client = match proxy_pool.as_ref() {
                    Some(pool) => pool.get_client(comp.proxy_profile_id.as_deref())?,
                    None => {
                        anyhow::ensure!(comp.proxy_profile_id.is_none(),
                            "configured proxy pool is unavailable; direct fallback refused");
                        client
                    }
                };
                anyhow::ensure!(
                    db.is_some() && job_id.is_some(),
                    "LANGUAGE_PACK_AUTHORITY_REQUIRED: database/job unavailable; no provider request sent"
                );

                // Translate each entry's msgid
                let mut translated_entries: Vec<I18nCallbackEntry> = Vec::new();
                for item in &lp_items {
                    if !try_reserve_discovery_item(&items_processed_total, max_items_per_run) {
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
                    match translate_language_pack_entry(
                        component_client,
                        comp,
                        db.as_ref(),
                        wp_base,
                        &effective_relation,
                        business_line,
                        subtype,
                        item,
                        &params,
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
                            let idempotency_key = language_pack_batch_idempotency_key(
                                wp_base,
                                relation.id,
                                business_line,
                                subtype,
                                &effective_relation.source_lang,
                                &effective_relation.target_lang,
                                &worker_config.worker_id,
                                std::slice::from_ref(translated),
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
                            let item_result = persist_language_pack_batch_for_review(
                                db_arc, jid, &data_dir, wp_base, relation, &effective_relation,
                                business_line, subtype, &lp_items,
                                std::slice::from_ref(translated), &idempotency_key, route_secret,
                                &i18n_payload, &comp.template.id, &params,
                            ).await;
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
                        anyhow::bail!(
                            "LANGUAGE_PACK_AUTHORITY_REQUIRED: review storage unavailable; no immediate callback"
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
                                log_file,
                                callback_sem,
                            )
                            .await
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
                            let device_id = worker_config.device_id.clone();
                            let idempotency_key = idempotency_key.clone();
                            let i18n_payload = i18n_payload.clone();
                            async move {
                                send_i18n_translation_callback(
                                    &client,
                                    &wp_base,
                                    &token,
                                    &worker_id,
                                    &device_id,
                                    &idempotency_key,
                                    &i18n_payload,
                                )
                                .await
                            }
                        },
                    )
                    .await
                    .map(|_| translated_entries.len())
                };
                match i18n_result {
                    Ok(count) => {
                        adaptive.on_success();
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
                        rel_processed += translated_entries.len();
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
            touch_last_run_at(&conn, &domain_key, relation.id)
                .context("confirm discovery last-run projection")?;
        }
        // Finalize job in DB (after normal content + language-pack branches)
        if let (Some(ref db_arc), Some(jid)) = (&db, *job_id) {
            let rc = rel_completed;
            let rf = rel_failed;
            let rp = rel_processed;
            let conn = db_arc.lock().await;
            // Existing items stay with their original job on dedup. An empty
            // successful scan owns no pending work; run attempts are not rows.
            let empty_status = if rf == 0 {
                "completed"
            } else if rc == 0 {
                "failed"
            } else {
                "partial"
            };
            match crate::db::jobs::project_job_from_items(&conn, jid, true, empty_status) {
                Ok(final_status) => {
                    let _ = log_event(
                        log_file,
                        "info",
                        "job.finalized",
                        json!({
                            "job_id": jid,
                            "relation_id": relation.id,
                            "status": final_status,
                            "completed": rc,
                            "failed": rf,
                            "processed": rp,
                        }),
                    );
                }
                Err(err) => {
                    let _ = log_event(log_file, "warning", "job.finalize_failed", json!({
                        "job_id": jid, "error": snippet(&format!("{err:#}")),
                    }));
                    return Ok((rel_processed, rel_completed, rel_failed + 1));
                }
            }
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

        Ok::<_, anyhow::Error>((rel_processed, rel_completed, rel_failed))
        }); // end relation_joins.spawn(async move { ... })
    } // end for relation in relations

    // Collect relation concurrency results
    let mut relation_storage_error = None;
    while let Some(join_result) = relation_joins.join_next().await {
        match join_result {
            Ok(Ok((p, c, f))) => {
                report.processed += p;
                report.completed += c;
                report.failed += f;
            }
            Ok(Err(err)) => {
                if relation_storage_error.is_none() {
                    relation_storage_error = Some(err);
                }
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

    // Drain siblings before surfacing storage failure: do not cancel a
    // concurrent provider request merely because another relation failed.
    if let Some(error) = relation_storage_error {
        return Err(error.context("discovery relation state unavailable"));
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