use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod startup;

use crate::auth::{
    build_request_id, is_auth_error_message, is_wp_token_rotation_error, parse_api_error_response,
    request_json_encrypted,
};
use crate::bindings::{
    build_verify_base_url, build_wp_base_url, classify_verify_outcome,
    domain_token_binding_local_sites, format_rfc3339_utc, gate, has_any_domain_token_bindings,
    load_component_bindings, load_components_local, load_domain_token_bindings,
    load_proxy_profiles, load_rule_component_bindings, load_task_type_component_bindings,
    normalize_api_base_url_key, normalize_domain_base, now_unix, resolve_entry_for_domain,
    resolve_local_dev_binding, resolve_route_secret_for_domain, resolve_wp_client_token_for_domain,
    save_domain_token_bindings, verify_identity, GateVerdict, VerifyOutcome,
};
use crate::component_rt::loader::{
    collect_configured_runtime_component_ids, load_component_runtimes,
};
use crate::component_rt::proxy::ProxyClientPool;
use crate::component_rt::sign_plugin;
use crate::config::*;
use crate::logging::{init_runtime_log_file, log_event, maybe_export_log, snippet};

/// Apply persisted log settings (Settings page state in SQLite) plus the
/// `WPTSALL_LOG_ENABLED` env override to the global logging atomics.
///
/// Worker mode must honor the same switch as the WebUI: release default is
/// logging DISABLED, an explicit DB `true` enables it, and the env var
/// overrides both when set.
fn apply_log_settings_from_db(db_path: &str) {
    // Register the process-wide log path first: engine modules without a
    // threaded log_file parameter (sync credentials, job bookkeeping) emit
    // their audit events through log_event_global.
    crate::logging::set_log_file_path(crate::config::log_file_path());
    let (db_enabled, db_level) = match crate::db::open_db(db_path) {
        Ok(conn) => (
            crate::db::system::get_system_config(&conn, "log_enabled"),
            crate::db::system::get_system_config(&conn, "log_min_level"),
        ),
        Err(_) => (None, None),
    };
    let enabled = crate::logging::resolve_log_enabled(
        db_enabled.as_deref(),
        std::env::var("WPTSALL_LOG_ENABLED").ok().as_deref(),
    );
    let level = db_level.unwrap_or_else(|| "info".to_string());
    crate::logging::set_log_enabled(enabled);
    crate::logging::set_log_min_level(&level);
}
use crate::persistence::PendingCallbackStore;
use crate::task_engine::discoverer::discover_and_translate;
use crate::types::*;

const CLIENT_MODE_HEADER: &str = "X-WPTSALL-Client-Mode";
const CLIENT_MODE_DESKTOP: &str = "desktop-client";
const LOCAL_DEV_LICENSE_STATUS: &str = "local_dev";

fn is_fatal_control_plane_error(err_text: &str) -> bool {
    is_auth_error_message(err_text) || err_text.contains("RATE_LIMITED")
}

pub fn build_worker_config(device_id: &str) -> WorkerConfig {
    let mut task_pull_statuses = parse_csv_env("WPTSALL_TASK_PULL_STATUSES");
    if task_pull_statuses.is_empty() {
        task_pull_statuses = vec!["pending".to_string(), "retry".to_string()];
    }
    WorkerConfig {
        worker_id: env_or("WPTSALL_WORKER_ID", device_id),
        device_id: device_id.to_string(),
        discovery_max_items_per_run: env_usize("WPTSALL_DISCOVERY_MAX_ITEMS_PER_RUN", 100),
        task_pull_statuses,
        task_concurrency: env_usize("WPTSALL_TASK_CONCURRENCY", 3).max(1),
        retry_max: env_u32("WPTSALL_RETRY_MAX", 2),
        retry_base_ms: env_u64("WPTSALL_RETRY_BASE_MS", 400).max(1),
        retry_max_ms: env_u64("WPTSALL_RETRY_MAX_MS", 5_000).max(1),
        component_fallback_enabled: env_bool("WPTSALL_COMPONENT_FALLBACK", false),
        default_max_input_chars: env_u64(
            "WPTSALL_DEFAULT_MAX_INPUT_CHARS",
            DEFAULT_MAX_INPUT_CHARS,
        ),
        default_split_strategy: env_or("WPTSALL_DEFAULT_SPLIT_STRATEGY", DEFAULT_SPLIT_STRATEGY),
        discovery_mode: env_bool("WPTSALL_DISCOVERY_MODE", true),
        review_mode: env_bool("WPTSALL_REVIEW_MODE", false),
    }
}

/// Entry point: local-first worker loop.
///
/// The official-site control plane is no longer the default runtime path. Set
/// WPTSALL_USE_SERVER_CONTROL_PLANE=1 only for legacy server/OAuth regression
/// runs.
pub async fn run_worker_cli(shutdown_token: CancellationToken) -> anyhow::Result<()> {
    if server_control_plane_enabled() {
        run_server_worker(shutdown_token).await
    } else {
        run_local_worker(shutdown_token).await
    }
}

// ---------------------------------------------------------------------------
// Local WP sites mode (default)
// ---------------------------------------------------------------------------

async fn run_local_worker(shutdown_token: CancellationToken) -> anyhow::Result<()> {
    let db_path = crate::config::db_path();
    let runtime_lease = crate::db::runtime::RuntimeLease::acquire(&db_path)?;
    let bootstrap_db = crate::db::open_db(&db_path)?;
    let device_id = crate::db::with_recovery_credit(&bootstrap_db, || {
        crate::db::system::migrate_device_id_from_file(&bootstrap_db)?;
        let device_id = match crate::config::wp_device_id_override() {
            Some(id) => id,
            None => crate::db::system::load_or_create_device_id(&bootstrap_db)?,
        };
        // S3/SEC-02 (07 audit, 12 批 A5): the resolved identity is the default
        // at-rest encryption source (see bindings::crypto::set_default_device_id).
        crate::bindings::set_default_device_id(&device_id);
        crate::db::migrate_from_json_if_needed(&bootstrap_db)?;
        crate::db::runtime::recover_interrupted_work(&bootstrap_db, &runtime_lease)
            .context("Failed to recover interrupted client work")?;
        Ok(device_id)
    })?;
    let db = Arc::new(tokio::sync::Mutex::new(bootstrap_db));
    let domain_token_bindings_path = crate::config::domain_token_bindings_file();
    let poll_seconds = env_u64("WPTSALL_POLL_SECONDS", 20).max(1);
    let one_shot = env_bool("WPTSALL_ONESHOT", false);
    let component_runtime_enabled = env_bool("WPTSALL_COMPONENT_RUNTIME", true);
    let component_id_override = env_or("WPTSALL_COMPONENT_ID", "");
    let component_prefer_ids = parse_csv_env("WPTSALL_COMPONENT_PREFER_IDS");
    let component_bindings_path = crate::config::component_bindings_file();
    let components_local_path = crate::config::components_local_file();
    let task_type_component_bindings_path = crate::config::task_type_component_bindings_file();
    let rule_component_bindings_path = crate::config::rule_component_bindings_file();
    let worker_config = build_worker_config(&device_id);
    let log_file = crate::config::log_file_path();
    let log_export_path = env_or("WPTSALL_LOG_EXPORT_PATH", "");
    apply_log_settings_from_db(&db_path);
    init_runtime_log_file(&log_file)?;
    // Boot observes durable async jobs without age-deleting saved work.
    if let Ok(conn) = crate::db::open_db(&db_path) {
        match crate::db::async_jobs::async_jobs_inventory(&conn) {
            Ok((failed, polling)) => {
                if failed > 0 || polling > 0 {
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.async_jobs_recovery_inventory",
                        json!({
                            "failed_kept": failed,
                            "polling_resumable": polling,
                        }),
                    );
                }
            }
            Err(err) => {
                eprintln!("warning: async jobs boot sweep failed: {err}");
            }
        }
        // Inventory is read-only; unresolved relay snapshots never expire.
        match crate::db::sync_inflight::sync_inflight_inventory(&conn) {
            Ok(shipping) => {
                if shipping > 0 {
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.sync_inflight_recovery_inventory",
                        json!({
                            "shipping_resumable": shipping,
                        }),
                    );
                }
            }
            Err(err) => {
                return Err(err.context("sync inflight recovery inventory failed"));
            }
        }
    }

    let proxy_pool = {
        // P7 unification: proxy profiles resolve through the config.rs
        // single authority, never derived from the bindings file directory.
        let proxy_profiles_path = crate::config::proxy_profiles_file();
        let profiles = crate::bindings::read_proxy_profiles(&proxy_profiles_path)
            .map_err(|_| crate::component_rt::loader::RuntimeConfigurationFault::error("proxy profiles"))?;
        Some(Arc::new(ProxyClientPool::new(&profiles.profiles)
            .map_err(|_| crate::component_rt::loader::RuntimeConfigurationFault::error("proxy profiles"))?))
    };
    let pending_callback_store = Arc::new(Mutex::new(PendingCallbackStore::open(&db_path)?));

    let mut domain_token_bindings = load_domain_token_bindings(&domain_token_bindings_path)
        .context("worker site binding authority is unreadable; startup refused")?;
    let mut task_type_component_bindings =
        load_task_type_component_bindings(&task_type_component_bindings_path)
            .context("worker task binding authority is unreadable; startup refused")?;
    let mut rule_component_bindings =
        load_rule_component_bindings(&rule_component_bindings_path)
            .context("worker rule binding authority is unreadable; startup refused")?;

    let _ = log_event(
        &log_file,
        "info",
        "startup",
        json!({
            "mode": "local",
            "device_id": device_id,
            "poll_seconds": poll_seconds,
            "one_shot": one_shot,
            "worker_id": worker_config.worker_id,
            "component_runtime_enabled": component_runtime_enabled,
            "component_id_override": component_id_override,
            "component_prefer_ids": component_prefer_ids,
            "domain_token_bindings_path": domain_token_bindings_path,
            "domain_token_bindings_count": domain_token_bindings.domains.len(),
            "db_path": db_path,
            "discovery_mode": worker_config.discovery_mode
        }),
    );

    if !component_runtime_enabled && !worker_config.component_fallback_enabled {
        let _ = log_event(
            &log_file,
            "error",
            "config.invalid_execution_backend",
            json!({
                "component_runtime_enabled": component_runtime_enabled,
                "component_fallback_enabled": worker_config.component_fallback_enabled,
                "message": "component runtime is disabled and fallback is disabled"
            }),
        );
        eprintln!(
            "{}",
            crate::i18n::t("cli.invalid_config", &crate::i18n::detect_cli_locale())
        );
        return Ok(());
    }

    let client = crate::auth::wp_http_client_builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    // 批 Q (事件驱动 wake): long-poll waiter per bound site — ready events
    // break the poll sleep below immediately. Polling stays the fallback
    // (waiter error/404/禁用 all degrade to the plain poll cadence);
    // one-shot runs never spawn a waiter.
    let event_wait_cfg = crate::task_engine::event_waiter::EventWaitConfig::from_env();
    let event_wake = Arc::new(tokio::sync::Notify::new());
    let mut event_waiter_handle: Option<tokio::task::JoinHandle<()>> = None;
    if event_wait_cfg.enabled && !one_shot {
        event_waiter_handle = Some(tokio::spawn(
            crate::task_engine::event_waiter::run_event_waiter(
                event_wait_cfg.clone(),
                worker_config.worker_id.clone(),
                device_id.clone(),
                log_file.clone(),
                Arc::clone(&event_wake),
                shutdown_token.clone(),
            ),
        ));
    }

    let (mut component_bindings, local_components_doc) = {
        let conn = db.lock().await;
        startup::load_component_documents(
            &conn,
            &domain_token_bindings,
            &component_bindings_path,
            &components_local_path,
        )?
    };
    let target_component_ids = collect_configured_runtime_component_ids(
        &local_components_doc,
        Some(&task_type_component_bindings),
        Some(&rule_component_bindings),
        Some(&component_bindings),
    );
    let component_registry = if component_runtime_enabled {
        match load_component_runtimes(
            &client,
            "",
            "",
            &log_file,
            &mut component_bindings,
            &component_bindings_path,
            Some(&target_component_ids),
            None,
        )
        .await
        {
            Ok(registry) => Some(Arc::new(registry)),
            Err(err) if err.is::<crate::component_rt::loader::RuntimeConfigurationFault>() => {
                return Err(err)
            }
            Err(err) => {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "component.runtime_disabled",
                    json!({ "reason": format!("{:#}", err), "mode": "local" }),
                );
                None
            }
        }
    } else {
        None
    };

    loop {
        if shutdown_token.is_cancelled() {
            let _ = log_event(&log_file, "info", "worker.shutdown_requested", json!({}));
            break;
        }

        domain_token_bindings = load_domain_token_bindings(&domain_token_bindings_path)
            .context("worker site binding authority changed or became unreadable")?;
        task_type_component_bindings = load_task_type_component_bindings(&task_type_component_bindings_path)
            .context("worker task binding authority changed or became unreadable")?;
        rule_component_bindings = load_rule_component_bindings(&rule_component_bindings_path)
            .context("worker rule binding authority changed or became unreadable")?;

        let domains = domain_token_binding_local_sites(&domain_token_bindings);
        let _ = log_event(
            &log_file,
            "info",
            "domains.local_synced",
            json!({ "count": domains.len() }),
        );

        let task_type_bindings_arc = Arc::new(task_type_component_bindings.clone());
        let rule_bindings_arc = Arc::new(rule_component_bindings.clone());
        let governor = Arc::new(crate::resource_governor::ResourceGovernor::from_env());

        struct DomainTask {
            api_base_url: String,
            wp_client_token: String,
            route_secret: Option<String>,
            wp_base: String,
            /// Identity Contract v1.1 §5 (C-1): verified identity this domain
            /// dispatches under; the lane-level check in the discoverer
            /// fail-closes on any disagreement.
            binding_identity: crate::types::PluginIdentity,
            relation_limit: Option<usize>,
        }
        let mut domain_tasks: Vec<DomainTask> = Vec::new();
        let mut missing_route_secret_domains: Vec<String> = Vec::new();
        let mut identity_bindings_dirty = false;

        for domain in &domains {
            let Some(token) = resolve_wp_client_token_for_domain(
                &domain.api_base_url,
                &domain_token_bindings,
                "",
            ) else {
                continue;
            };
            let route_secret =
                resolve_route_secret_for_domain(&domain.api_base_url, &domain_token_bindings)
                    .or_else(|| domain.route_secret.clone());
            let domain_base = normalize_domain_base(&domain.api_base_url);
            let Some(wp_base) = route_secret
                .as_deref()
                .and_then(|secret| build_wp_base_url(&domain_base, secret))
            else {
                missing_route_secret_domains.push(domain.api_base_url.clone());
                let _ = log_event(
                    &log_file,
                    "warning",
                    "domain.skipped_missing_route_secret",
                    json!({ "api_base_url": domain.api_base_url }),
                );
                continue;
            };

            // Identity Contract v1.1 §5 (C-1): fail-closed identity gate.
            // A binding whose plugin identity is unknown, mismatched, or past
            // the 24h verification TTL never dispatches tasks. Event payloads
            // carry no credential fields.
            let Some(entry) =
                resolve_entry_for_domain(&domain.api_base_url, &domain_token_bindings)
            else {
                continue;
            };
            let binding_identity = match gate(entry, now_unix()) {
                GateVerdict::Fresh(identity) => Some(identity),
                GateVerdict::Reverify { .. } => {
                    // Identity-aware verify base (contract §2/§3): the entry's
                    // stored identity (v3 migration default wpmmcc_ats)
                    // selects the endpoint family — ATS bindings verify on
                    // wptsall/v2/{secret}/client/ping, wpmmcc bindings on
                    // wpmmcc/v1/{secret}/sync/ping.
                    let expected_identity = entry
                        .plugin_identity
                        .unwrap_or(crate::types::PluginIdentity::WpmmccAts);
                    let verify_base = build_verify_base_url(
                        &domain_base,
                        route_secret.as_deref().unwrap_or(""),
                        &expected_identity,
                    )
                    .unwrap_or_else(|| wp_base.clone());
                    let outcome = verify_identity(
                        &client,
                        &verify_base,
                        &token,
                        &worker_config.worker_id,
                        &worker_config.device_id,
                        route_secret.as_deref(),
                    )
                    .await;
                    match classify_verify_outcome(&outcome, entry) {
                        Ok(verified) => {
                            // Refresh the persisted binding (contract §4):
                            // verified identity + timestamp + capability
                            // snapshot. First verify of a migrated v3 entry
                            // overwrites the migration default.
                            let caps = match &outcome {
                                VerifyOutcome::Verified(_, caps) => caps.clone(),
                                _ => crate::types::IdentityCapabilities::default(),
                            };
                            let verified_at = format_rfc3339_utc(now_unix());
                            let legacy_key = normalize_api_base_url_key(&domain.api_base_url);
                            let stored = match domain_token_bindings.domains.get_mut(&domain_base) {
                                Some(stored) => Some(stored),
                                None => domain_token_bindings.domains.get_mut(&legacy_key),
                            };
                            if let Some(stored) = stored {
                                stored.plugin_identity = Some(verified);
                                stored.identity_verified_at = Some(verified_at);
                                stored.identity_capabilities = Some(caps);
                                identity_bindings_dirty = true;
                            }
                            Some(verified)
                        }
                        Err(Some(code)) => {
                            let _ = log_event(
                                &log_file,
                                "warning",
                                code,
                                json!({ "api_base_url": domain.api_base_url }),
                            );
                            None
                        }
                        Err(None) => {
                            // Transport failure: not a reserved-code event;
                            // retry next cycle.
                            let _ = log_event(
                                &log_file,
                                "warning",
                                "identity.verify_failed",
                                json!({ "api_base_url": domain.api_base_url }),
                            );
                            None
                        }
                    }
                }
            };
            let Some(binding_identity) = binding_identity else {
                continue;
            };

            // FL-3 (Wave-2): this collector feeds ONLY the wpmmcc-ats
            // discovery lanes. A binding that verified as plain `wpmmcc`
            // is owned by the pair engine (sync lanes); dispatching it here
            // would trip the lane-entry identity guard on EVERY run-once
            // iteration (observed: 180-iteration identity_mismatch warn
            // storms in SIM-08/SIM-13). Filter at task generation; one
            // info notice per (domain, identity) keeps the exclusion
            // visible without warn-level radar noise. No credential fields.
            if binding_identity != crate::types::PluginIdentity::WpmmccAts {
                if crate::bindings::identity_exclusion_first_notice(
                    "wpmmcc_ats_task_generation",
                    &domain.api_base_url,
                    binding_identity.as_wire_str(),
                ) {
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.domain_lane_excluded",
                        json!({
                            "api_base_url": domain.api_base_url,
                            "lane": "wpmmcc_ats",
                            "binding_identity": binding_identity.as_wire_str(),
                        }),
                    );
                }
                continue;
            }

            domain_tasks.push(DomainTask {
                api_base_url: domain.api_base_url.clone(),
                wp_client_token: token,
                route_secret,
                wp_base,
                binding_identity,
                relation_limit: None,
            });
        }

        if identity_bindings_dirty {
            if let Err(err) =
                save_domain_token_bindings(&domain_token_bindings_path, &domain_token_bindings)
            {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "worker.identity_bindings_save_failed",
                    json!({
                        "path": domain_token_bindings_path,
                        "error": format!("{:#}", err)
                    }),
                );
            }
        }

        if domain_tasks.is_empty() {
            let _ = log_event(
                &log_file,
                "info",
                "worker.local.empty_domain_tasks",
                json!({
                    "domains_total": domains.len(),
                    "skipped_missing_route_secret": missing_route_secret_domains.len(),
                }),
            );
        }

        let mut domain_joins: tokio::task::JoinSet<anyhow::Result<Option<DomainRunReport>>> =
            tokio::task::JoinSet::new();
        for dt in domain_tasks {
            let client = client.clone();
            let log_file = log_file.clone();
            let component_registry = component_registry.clone();
            let proxy_pool = proxy_pool.clone();
            let component_id_override = component_id_override.clone();
            let component_prefer_ids = component_prefer_ids.clone();
            let task_type_bindings = Arc::clone(&task_type_bindings_arc);
            let rule_bindings = Arc::clone(&rule_bindings_arc);
            let worker_config = worker_config.clone();
            let pending_callback_store = Arc::clone(&pending_callback_store);
            let governor = Arc::clone(&governor);
            let domain_lease = Arc::clone(&runtime_lease);
            let db = Arc::clone(&db);

            domain_joins.spawn(async move {
                let _lease = domain_lease;
                let Some(_domain_permit) = governor.domain_sem.acquire().await.ok() else {
                    return Ok(None);
                };
                let result = discover_and_translate(
                    &client,
                    &dt.wp_base,
                    &dt.wp_client_token,
                    &log_file,
                    component_registry.clone(),
                    proxy_pool.clone(),
                    &component_id_override,
                    &component_prefer_ids,
                    Some(task_type_bindings.clone()),
                    Some(rule_bindings.as_ref()),
                    &worker_config,
                    dt.route_secret.as_deref(),
                    &pending_callback_store,
                    Some(Arc::clone(&governor.global_translation_sem)),
                    Some(Arc::clone(&governor.global_callback_sem)),
                    dt.relation_limit,
                    Some(Arc::clone(&db)),
                    dt.binding_identity,
                )
                .await;

                match result {
                    Ok(report) => Ok(Some(report)),
                    Err(err) => {
                        let err_text = format!("{:#}", err);
                        let _ = log_event(
                            &log_file,
                            "error",
                            "domain.process_failed",
                            json!({
                                "api_base_url": dt.api_base_url,
                                "error": err_text
                            }),
                        );
                        Ok(None)
                    }
                }
            });
        }

        let mut loop_reports: Vec<DomainRunReport> = Vec::new();
        while let Some(join_result) = domain_joins.join_next().await {
            match join_result {
                Ok(Ok(Some(report))) => loop_reports.push(report),
                Ok(Ok(None)) => {}
                Ok(Err(err)) => {
                    let _ = log_event(
                        &log_file,
                        "error",
                        "worker.domain_task_failed",
                        json!({ "error": format!("{:#}", err) }),
                    );
                }
                Err(err) => {
                    let _ = log_event(
                        &log_file,
                        "error",
                        "worker.domain_task_join_failed",
                        json!({ "error": err.to_string() }),
                    );
                }
            }
        }
        if !loop_reports.is_empty() {
            log_loop_summary(&log_file, &loop_reports);
        }
        if !missing_route_secret_domains.is_empty() {
            let _ = log_event(
                &log_file,
                "warning",
                "worker.domains_skipped_missing_route_secret",
                json!({
                    "count": missing_route_secret_domains.len(),
                    "domains": missing_route_secret_domains,
                }),
            );
        }

        // WPMMCC Cross-Site Sync Lane (WBS 4.2 / lane: "wpmmcc_sync").
        //
        // Every eligible pair runs as its own spawned task (true physical
        // concurrency — the old serial for-await was mislabeled as
        // "并发车道"), with per-site HMAC credentials from the pairing
        // handshake and per-pair translator runtimes for the
        // sync_and_translate cascade. Errors are logged, never swallowed.
        {
            let sync_pairs_path = crate::config::sync_pairs_file();
            let credentials_path = crate::config::sync_peer_credentials_file();
            let pairs_doc = match crate::sync_engine::load_sync_pairs(&sync_pairs_path) {
                Ok(doc) => doc,
                Err(err) => {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "worker.sync_pairs_load_failed",
                        json!({ "error": format!("{err:#}") }),
                    );
                    crate::sync_engine::SyncPairsDoc::default()
                }
            };
            let peer_credentials =
                match crate::sync_engine::load_peer_credentials(&credentials_path) {
                    Ok(doc) => doc,
                    Err(err) => {
                        let _ = log_event(
                            &log_file,
                            "warning",
                            "worker.sync_credentials_load_failed",
                            json!({ "error": format!("{err:#}") }),
                        );
                        crate::sync_engine::PeerCredentialsDoc::default()
                    }
                };

            let now_ts = crate::logging::unix_ts();
            let mut eligible: Vec<crate::sync_engine::SyncPair> = pairs_doc
                .pairs
                .into_iter()
                .filter(|pair| pair.status == crate::sync_engine::SyncPairStatus::Active)
                .filter(|pair| {
                    let should_run = match pair.sync_frequency {
                        crate::sync_engine::SyncFrequency::Manual => one_shot,
                        crate::sync_engine::SyncFrequency::EveryMinute => pair
                            .last_sync_at
                            .map(|t| now_ts.saturating_sub(t) >= 60)
                            .unwrap_or(true),
                        crate::sync_engine::SyncFrequency::Hourly => pair
                            .last_sync_at
                            .map(|t| now_ts.saturating_sub(t) >= 3600)
                            .unwrap_or(true),
                        crate::sync_engine::SyncFrequency::Daily => pair
                            .last_sync_at
                            .map(|t| now_ts.saturating_sub(t) >= 86400)
                            .unwrap_or(true),
                    };
                    // Skip pairs whose ends are not paired yet — the run
                    // would only record a config error every cycle.
                    should_run
                        && crate::sync_engine::find_peer_credential(
                            &peer_credentials,
                            &pair.source_domain,
                        )
                        .is_some()
                        && crate::sync_engine::find_peer_credential(
                            &peer_credentials,
                            &pair.target_domain,
                        )
                        .is_some()
                        // 批 C: sync-pair cooldown (X-5). A pair whose last
                        // run was a TOTAL failure (zero synced, ≥1 error)
                        // waits out its cooldown instead of retrying a dead
                        // endpoint at full worker cadence. The skip is itself
                        // logged — the evidence line ops greps for.
                        && {
                            let decision =
                                crate::task_engine::backoff::check_sync_pair(&pair.id);
                            if decision.blocked {
                                let _ = log_event(
                                    &log_file,
                                    "info",
                                    "worker.sync_pair_backoff_skip",
                                    json!({
                                        "pair_id": pair.id,
                                        "remaining_secs": decision.remaining_secs,
                                        "consecutive_failures": decision.consecutive_failures,
                                    }),
                                );
                                false
                            } else {
                                true
                            }
                        }
                })
                .collect();

            if !eligible.is_empty() {
                let _ = log_event(
                    &log_file,
                    "info",
                    "worker.sync_lane_started",
                    json!({ "pairs": eligible.len(), "one_shot": one_shot }),
                );
                let mut lane = tokio::task::JoinSet::new();
                for pair in eligible.drain(..) {
                    let client = client.clone();
                    let log_file = log_file.clone();
                    let credentials = peer_credentials.clone();
                    let pair_lease = Arc::clone(&runtime_lease);
                    lane.spawn(async move {
                        let _lease = pair_lease;
                        let pair_id = pair.id.clone();
                        let translator = match pair.sync_mode {
                            crate::sync_engine::SyncMode::SyncAndTranslate => {
                                let component_id =
                                    pair.translate_component_id.as_deref().unwrap_or("").trim();
                                if component_id.is_empty() {
                                    None // engine records the config error
                                } else {
                                    match crate::sync_engine::build_translator(
                                        &client,
                                        component_id,
                                        &log_file,
                                    )
                                    .await
                                    {
                                        Ok(t) => Some(t),
                                        Err(err) => {
                                            let _ = log_event(
                                                &log_file,
                                                "warning",
                                                "worker.sync_translator_unavailable",
                                                json!({
                                                    "pair_id": pair_id,
                                                    "component_id": component_id,
                                                    "error": format!("{err:#}"),
                                                }),
                                            );
                                            None
                                        }
                                    }
                                }
                            }
                            crate::sync_engine::SyncMode::SyncOnly => None,
                        };
                        let result = crate::sync_engine::sync_pair_run(
                            &client,
                            &pair_id,
                            &credentials,
                            translator.as_ref(),
                            &log_file,
                        )
                        .await;
                        (pair_id, result)
                    });
                }
                while let Some(joined) = lane.join_next().await {
                    match joined {
                        Ok((pair_id, Ok(report))) => {
                            // 批 C (X-5): feed the sync-pair backoff domain.
                            // Only a TOTAL failure (zero synced, ≥1 error)
                            // cools the pair down; any success resets it.
                            if crate::sync_engine::sync_run_is_total_failure(&report) {
                                let decision =
                                    crate::task_engine::backoff::record_sync_pair_failure(&pair_id);
                                let _ = log_event(
                                    &log_file,
                                    "warning",
                                    "worker.sync_pair_backoff_recorded",
                                    json!({
                                        "pair_id": pair_id,
                                        "trace_id": report.trace_id,
                                        "cooldown_secs": decision.remaining_secs,
                                        "consecutive_failures": decision.consecutive_failures,
                                        "last_error": report.last_error,
                                    }),
                                );
                            } else {
                                crate::task_engine::backoff::record_sync_pair_success(&pair_id);
                            }
                            if report.error_count > 0 {
                                let _ = log_event(
                                    &log_file,
                                    "warning",
                                    "worker.sync_pair_finished_with_errors",
                                    json!({
                                        "pair_id": pair_id,
                                        "trace_id": report.trace_id,
                                        "errors": report.error_count,
                                        "last_error": report.last_error,
                                    }),
                                );
                            }
                        }
                        Ok((pair_id, Err(err))) => {
                            let _ = log_event(
                                &log_file,
                                "error",
                                "worker.sync_pair_failed",
                                json!({ "pair_id": pair_id, "error": format!("{err:#}") }),
                            );
                        }
                        Err(err) => {
                            let _ = log_event(
                                &log_file,
                                "error",
                                "worker.sync_pair_join_failed",
                                json!({ "error": err.to_string() }),
                            );
                        }
                    }
                }
            }
        }

        if one_shot || shutdown_token.is_cancelled() {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(poll_seconds)) => {}
            // 批 Q: waiter 就绪信号提前结束本轮 sleep（Notify permit 语义
            // 保证 drain 期间的多次唤醒至多补一轮，不惊群）。
            _ = event_wake.notified() => {
                let _ = log_event(
                    &log_file,
                    "info",
                    "worker.event_wake_break",
                    json!({ "poll_seconds": poll_seconds }),
                );
            }
            _ = shutdown_token.cancelled() => {
                let _ = log_event(&log_file, "info", "worker.shutdown_during_sleep", json!({}));
                break;
            }
        }
    }

    if let Some(handle) = event_waiter_handle.take() {
        handle.abort();
    }
    maybe_export_log(&log_file, &log_export_path)?;
    let _ = log_event(&log_file, "info", "shutdown", json!({ "mode": "local" }));
    crate::logging::flush_log();
    Ok(())
}

// ---------------------------------------------------------------------------
// Server mode (legacy, opt-in)
// ---------------------------------------------------------------------------

async fn run_server_worker(shutdown_token: CancellationToken) -> anyhow::Result<()> {
    let server_base = configured_server_base();
    if server_base.is_empty() {
        anyhow::bail!(
            "server control-plane mode requires WPTSALL_SERVER_BASE or WPTSALL_SERVER_URL; no implicit endpoint is used"
        );
    }
    // Warn when Server base URL uses plain HTTP: the session token is sent in the
    // X-Client-Session header and would be transmitted in cleartext.
    if server_base.starts_with("http://")
        && !server_base.starts_with("http://127.")
        && !server_base.starts_with("http://localhost")
    {
        eprintln!(
            "WARNING: WPTSALL_SERVER_BASE uses plain HTTP ({}). \
            The session token will be transmitted without TLS encryption. \
            Use https:// in production.",
            server_base
        );
    }
    let db_path = crate::config::db_path();
    let runtime_lease = crate::db::runtime::RuntimeLease::acquire(&db_path)?;
    let bootstrap_db = crate::db::open_db(&db_path)?;
    let device_id = crate::db::with_recovery_credit(&bootstrap_db, || {
        crate::db::system::migrate_device_id_from_file(&bootstrap_db)?;
        let device_id = match crate::config::wp_device_id_override() {
            Some(id) => id,
            None => crate::db::system::load_or_create_device_id(&bootstrap_db)?,
        };
        // S3/SEC-02 (07 audit, 12 批 A5): the resolved identity is the default
        // at-rest encryption source (see bindings::crypto::set_default_device_id).
        crate::bindings::set_default_device_id(&device_id);
        crate::db::migrate_from_json_if_needed(&bootstrap_db)?;
        crate::db::runtime::recover_interrupted_work(&bootstrap_db, &runtime_lease)
            .context("Failed to recover interrupted client work")?;
        Ok(device_id)
    })?;
    let db = Arc::new(tokio::sync::Mutex::new(bootstrap_db));
    let wp_client_token_fallback = env_or("WPTSALL_WP_CLIENT_TOKEN", "");
    let domain_token_bindings_path = crate::config::domain_token_bindings_file();
    let poll_seconds = env_u64("WPTSALL_POLL_SECONDS", 20).max(1);
    let one_shot = env_bool("WPTSALL_ONESHOT", false);
    let component_runtime_enabled = env_bool("WPTSALL_COMPONENT_RUNTIME", true);
    let component_id_override = env_or("WPTSALL_COMPONENT_ID", "");
    let component_prefer_ids = parse_csv_env("WPTSALL_COMPONENT_PREFER_IDS");
    let component_bindings_path = crate::config::component_bindings_file();
    let components_local_path = crate::config::components_local_file();
    let task_type_component_bindings_path = crate::config::task_type_component_bindings_file();
    let rule_component_bindings_path = crate::config::rule_component_bindings_file();
    let worker_config = build_worker_config(&device_id);
    let log_file = crate::config::log_file_path();
    let log_export_path = env_or("WPTSALL_LOG_EXPORT_PATH", "");
    let proxy_pool = {
        // P7 unification: proxy profiles resolve through the config.rs
        // single authority, never derived from the bindings file directory.
        let proxy_profiles_path = crate::config::proxy_profiles_file();
        match load_proxy_profiles(&proxy_profiles_path)
            .and_then(|doc| ProxyClientPool::new(&doc.profiles))
        {
            Ok(pool) => Some(Arc::new(pool)),
            Err(err) => {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "worker.proxy_pool_failed",
                    json!({
                        "path": proxy_profiles_path,
                        "error": format!("{:#}", err)
                    }),
                );
                None
            }
        }
    };
    let pending_callback_store = Arc::new(Mutex::new(PendingCallbackStore::open(&db_path)?));
    let recovered_pending_callbacks = {
        let pending = {
            let store = pending_callback_store.lock().await;
            store.entry_count_checked()?
        };
        let conn = crate::db::open_db(&db_path)?;
        let translated = crate::db::jobs::count_all_items_by_status(&conn, "translated")?;
        pending
            .checked_add(usize::try_from(translated)?)
            .context("recovered callback count overflow")?
    };

    apply_log_settings_from_db(&db_path);
    init_runtime_log_file(&log_file)?;
    // Non-destructive async recovery inventory in the server lane too.
    if let Ok(conn) = crate::db::open_db(&db_path) {
        match crate::db::async_jobs::async_jobs_inventory(&conn) {
            Ok((failed, polling)) => {
                if failed > 0 || polling > 0 {
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.async_jobs_recovery_inventory",
                        json!({
                            "failed_kept": failed,
                            "polling_resumable": polling,
                        }),
                    );
                }
            }
            Err(err) => {
                eprintln!("warning: async jobs boot sweep failed: {err}");
            }
        }
        match crate::db::sync_inflight::sync_inflight_inventory(&conn) {
            Ok(shipping) => {
                if shipping > 0 {
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.sync_inflight_recovery_inventory",
                        json!({
                            "shipping_resumable": shipping,
                        }),
                    );
                }
            }
            Err(err) => {
                return Err(err.context("sync inflight recovery inventory failed"));
            }
        }
    }
    let mut domain_token_bindings = match load_domain_token_bindings(&domain_token_bindings_path) {
        Ok(doc) => doc,
        Err(err) => {
            let _ = log_event(
                &log_file,
                "warning",
                "worker.domain_tokens_load_failed",
                json!({
                    "path": domain_token_bindings_path,
                    "error": format!("{:#}", err)
                }),
            );
            DomainTokenBindingsDoc::default()
        }
    };
    let mut task_type_component_bindings =
        match load_task_type_component_bindings(&task_type_component_bindings_path) {
            Ok(doc) => doc,
            Err(err) => {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "worker.task_type_components_load_failed",
                    json!({
                        "path": task_type_component_bindings_path,
                        "error": format!("{:#}", err)
                    }),
                );
                TaskTypeComponentBindingsDoc::default()
            }
        };
    let mut rule_component_bindings =
        match load_rule_component_bindings(&rule_component_bindings_path) {
            Ok(doc) => doc,
            Err(err) => {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "worker.rule_component_bindings_load_failed",
                    json!({
                        "path": rule_component_bindings_path,
                        "error": format!("{:#}", err)
                    }),
                );
                RuleComponentBindingsDoc::default()
            }
        };
    let mut last_domain_token_reload_error = String::new();
    let mut last_task_type_component_reload_error = String::new();
    let mut last_rule_component_reload_error = String::new();
    let _ = log_event(
        &log_file,
        "info",
        "startup",
        json!({
            "mode": "server",
            "server_base": server_base,
            "device_id": device_id,
            "poll_seconds": poll_seconds,
            "one_shot": one_shot,
            "worker_id": worker_config.worker_id,
            "component_runtime_enabled": component_runtime_enabled,
            "component_id_override": component_id_override,
            "component_prefer_ids": component_prefer_ids,
            "component_bindings_path": component_bindings_path,
            "domain_token_bindings_path": domain_token_bindings_path,
            "domain_token_bindings_count": domain_token_bindings.domains.len(),
            "task_type_component_bindings_path": task_type_component_bindings_path,
            "task_type_component_bindings_count": task_type_component_bindings.task_types.len(),
            "rule_component_bindings_path": rule_component_bindings_path,
            "task_pull_statuses": worker_config.task_pull_statuses,
            "task_concurrency": worker_config.task_concurrency,
            "retry_max": worker_config.retry_max,
            "retry_base_ms": worker_config.retry_base_ms,
            "retry_max_ms": worker_config.retry_max_ms,
            "component_fallback_enabled": worker_config.component_fallback_enabled,
            "db_path": db_path,
            "recovered_pending_callbacks": recovered_pending_callbacks,
            "discovery_mode": worker_config.discovery_mode
        }),
    );

    // Sign plugin path (loaded after authentication to allow auto-download)
    let sign_plugin_path = env_or(
        "WPTSALL_SIGN_PLUGIN_PATH",
        sign_plugin::DEFAULT_SIGN_PLUGIN_PATH,
    );

    if !component_runtime_enabled && !worker_config.component_fallback_enabled {
        let _ = log_event(
            &log_file,
            "error",
            "config.invalid_execution_backend",
            json!({
                "component_runtime_enabled": component_runtime_enabled,
                "component_fallback_enabled": worker_config.component_fallback_enabled,
                "message": "component runtime is disabled and fallback is disabled"
            }),
        );
        eprintln!(
            "{}",
            crate::i18n::t("cli.invalid_config", &crate::i18n::detect_cli_locale())
        );
        return Ok(());
    }

    if wp_client_token_fallback.trim().is_empty()
        && !has_any_domain_token_bindings(&domain_token_bindings)
    {
        eprintln!(
            "{}",
            crate::i18n::t("cli.missing_wp_token", &crate::i18n::detect_cli_locale())
        );
        let _ = log_event(
            &log_file,
            "error",
            "config.missing_wp_client_token",
            json!({
                "fallback_present": false,
                "domain_bindings_present": false
            }),
        );
        return Ok(());
    }

    let client = crate::auth::wp_http_client_builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    let mut session_token =
        match crate::oauth::cli_oauth_login(&client, &server_base, &device_id, &log_file, &db_path)
            .await
        {
            Ok(token) => token,
            Err(err) => {
                eprintln!(
                    "{}: {}",
                    crate::i18n::t("cli.login_failed", &crate::i18n::detect_cli_locale()),
                    err
                );
                let _ = log_event(
                    &log_file,
                    "error",
                    "auth.login_failed",
                    json!({ "error": format!("{:#}", err) }),
                );
                return Ok(());
            }
        };

    // Auto-fetch signing public key from Server → persist in DB (non-fatal).
    // Required for component template signature verification.
    {
        let key_url = format!("{}/api/v1/client/signing-public-key", server_base);
        match crate::auth::request_json_encrypted::<crate::types::ApiResponse<serde_json::Value>>(
            client
                .get(&key_url)
                .header("X-Client-Session", &session_token),
            "signing-public-key",
            &session_token,
        )
        .await
        {
            Ok(resp) if resp.success => {
                if let Some(pem) = resp.data.get("public_key_pem").and_then(|v| v.as_str()) {
                    if pem.contains("BEGIN PUBLIC KEY") {
                        let key_id = resp
                            .data
                            .get("key_id")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|v| !v.is_empty());
                        if let Ok(conn) = crate::db::open_db(&db_path) {
                            let _ = crate::db::system::set_signing_key_material(&conn, pem, key_id);
                        }
                        let _ = log_event(
                            &log_file,
                            "info",
                            "component.signing_key_fetched",
                            json!({ "key_id": key_id.unwrap_or("unknown") }),
                        );
                    }
                }
            }
            Ok(_) => {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "component.signing_key_fetch_failed",
                    json!({ "note": "server returned success=false" }),
                );
            }
            Err(err) => {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "component.signing_key_fetch_failed",
                    json!({ "error": format!("{:#}", err) }),
                );
            }
        }
    }

    // Read signing key from DB for component loader and sign plugin download.
    let signing_key_from_db = crate::db::open_db(&db_path)
        .ok()
        .and_then(|conn| crate::db::system::get_signing_key(&conn))
        .filter(|pem| pem.trim().starts_with("-----BEGIN PUBLIC KEY-----"));
    let signing_key_id_from_db = crate::db::open_db(&db_path)
        .ok()
        .and_then(|conn| crate::db::system::get_signing_key_id(&conn))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    // Auto-download sign plugin from server (optional, non-fatal)
    {
        match sign_plugin::auto_download_sign_plugin(
            &client,
            &server_base,
            &session_token,
            &sign_plugin_path,
            signing_key_from_db.as_deref(),
            signing_key_id_from_db.as_deref(),
            &log_file,
        )
        .await
        {
            Ok(true) => {
                let _ = log_event(
                    &log_file,
                    "info",
                    "sign_plugin.auto_downloaded",
                    json!({ "path": sign_plugin_path }),
                );
            }
            Ok(false) => {} // up-to-date or not available
            Err(err) => {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "sign_plugin.auto_download_failed",
                    json!({ "error": format!("{:#}", err) }),
                );
            }
        }
    }

    // Load WASM sign plugin from local file (optional -- all algorithms have built-in fallbacks)
    let sign_plugin_loaded = sign_plugin::init_sign_plugin(&sign_plugin_path);
    let _ = log_event(
        &log_file,
        "info",
        if sign_plugin_loaded {
            "sign_plugin.loaded"
        } else {
            "sign_plugin.not_found"
        },
        json!({
            "path": sign_plugin_path,
            "loaded": sign_plugin_loaded
        }),
    );

    let mut component_bindings = load_component_bindings(&component_bindings_path)?;
    let local_components_doc = load_components_local(&components_local_path).map_err(|_| {
        crate::component_rt::loader::RuntimeConfigurationFault::error("local components file")
    })?;
    let target_component_ids = collect_configured_runtime_component_ids(
        &local_components_doc,
        Some(&task_type_component_bindings),
        Some(&rule_component_bindings),
        Some(&component_bindings),
    );
    let component_registry = if component_runtime_enabled {
        match load_component_runtimes(
            &client,
            &server_base,
            &session_token,
            &log_file,
            &mut component_bindings,
            &component_bindings_path,
            Some(&target_component_ids),
            signing_key_from_db.as_deref(),
        )
        .await
        {
            Ok(registry) => Some(Arc::new(registry)),
            Err(err) if err.is::<crate::component_rt::loader::RuntimeConfigurationFault>() => {
                return Err(err)
            }
            Err(err) => {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "component.runtime_disabled",
                    json!({ "reason": format!("{:#}", err) }),
                );
                None
            }
        }
    } else {
        let _ = log_event(
            &log_file,
            "info",
            "component.runtime_disabled",
            json!({ "reason": "WPTSALL_COMPONENT_RUNTIME=0" }),
        );
        None
    };
    let mut consecutive_hb_failures: u32 = 0;

    loop {
        if shutdown_token.is_cancelled() {
            let _ = log_event(&log_file, "info", "worker.shutdown_requested", json!({}));
            break;
        }

        match load_domain_token_bindings(&domain_token_bindings_path) {
            Ok(doc) => {
                domain_token_bindings = doc;
                if !last_domain_token_reload_error.is_empty() {
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.domain_tokens_reload_recovered",
                        json!({
                            "path": domain_token_bindings_path
                        }),
                    );
                    last_domain_token_reload_error.clear();
                }
            }
            Err(err) => {
                let err_text = format!("{:#}", err);
                if err_text != last_domain_token_reload_error {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "worker.domain_tokens_reload_failed",
                        json!({
                            "path": domain_token_bindings_path,
                            "error": err_text
                        }),
                    );
                    last_domain_token_reload_error = err_text;
                }
            }
        }
        match load_task_type_component_bindings(&task_type_component_bindings_path) {
            Ok(doc) => {
                task_type_component_bindings = doc;
                if !last_task_type_component_reload_error.is_empty() {
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.task_type_components_reload_recovered",
                        json!({
                            "path": task_type_component_bindings_path
                        }),
                    );
                    last_task_type_component_reload_error.clear();
                }
            }
            Err(err) => {
                let err_text = format!("{:#}", err);
                if err_text != last_task_type_component_reload_error {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "worker.task_type_components_reload_failed",
                        json!({
                            "path": task_type_component_bindings_path,
                            "error": err_text
                        }),
                    );
                    last_task_type_component_reload_error = err_text;
                }
            }
        }
        match load_rule_component_bindings(&rule_component_bindings_path) {
            Ok(doc) => {
                rule_component_bindings = doc;
                if !last_rule_component_reload_error.is_empty() {
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.rule_component_bindings_reload_recovered",
                        json!({
                            "path": rule_component_bindings_path
                        }),
                    );
                    last_rule_component_reload_error.clear();
                }
            }
            Err(err) => {
                let err_text = format!("{:#}", err);
                if err_text != last_rule_component_reload_error {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "worker.rule_component_bindings_reload_failed",
                        json!({
                            "path": rule_component_bindings_path,
                            "error": err_text
                        }),
                    );
                    last_rule_component_reload_error = err_text;
                }
            }
        }

        let mut headers = HeaderMap::new();
        headers.insert("X-Client-Session", session_token.parse()?);
        headers.insert(CLIENT_MODE_HEADER, CLIENT_MODE_DESKTOP.parse()?);

        // heartbeat
        let hb_url = format!("{}/api/v1/client/heartbeat", server_base);
        let hb_resp = client
            .post(hb_url)
            .headers(headers.clone())
            .header(REQUEST_ID_HEADER, build_request_id("heartbeat"))
            .json(&json!({
                "device_id": device_id,
                "client_version": env!("CARGO_PKG_VERSION")
            }))
            .send()
            .await?;
        let hb_status = hb_resp.status();
        let hb_body = hb_resp.text().await?;
        if !hb_status.is_success() {
            let hb_error = parse_api_error_response(&hb_body);
            let hb_error_code = hb_error
                .as_ref()
                .map(|e| e.error.code.clone())
                .unwrap_or_default();
            let hb_error_message = hb_error
                .as_ref()
                .map(|e| e.error.message.clone())
                .unwrap_or_default();
            let should_relogin = hb_status.as_u16() == 401
                || hb_error_code == "SESSION_REVOKED"
                || hb_error_code == "SESSION_EXPIRED";
            let _ = log_event(
                &log_file,
                if should_relogin { "warning" } else { "error" },
                "heartbeat.failed",
                json!({
                    "status": hb_status.as_u16(),
                    "error_code": hb_error_code,
                    "error_message": hb_error_message,
                    "body": snippet(&hb_body)
                }),
            );

            if should_relogin {
                let locale = crate::i18n::detect_cli_locale();
                eprintln!("{}", crate::i18n::t("cli.session_expired_relogin", &locale));
                let _ = log_event(
                    &log_file,
                    "warning",
                    "auth.session_expired_attempting_relogin",
                    json!({ "reason": "heartbeat_unauthorized" }),
                );
                match crate::oauth::cli_oauth_login(
                    &client,
                    &server_base,
                    &device_id,
                    &log_file,
                    &db_path,
                )
                .await
                {
                    Ok(new_token) => {
                        session_token = new_token;
                        consecutive_hb_failures = 0;
                        let _ = log_event(&log_file, "info", "auth.relogin_ok", json!({}));
                        continue;
                    }
                    Err(relogin_err) => {
                        eprintln!(
                            "{}: {}",
                            crate::i18n::t("cli.login_failed", &locale),
                            relogin_err
                        );
                        let _ = log_event(
                            &log_file,
                            "error",
                            "auth.relogin_failed",
                            json!({"error": format!("{:#}", relogin_err)}),
                        );
                        break;
                    }
                }
            } else {
                consecutive_hb_failures += 1;
                eprintln!(
                    "{}: {} (attempt {}/5)",
                    crate::i18n::t("cli.heartbeat_failed", &crate::i18n::detect_cli_locale()),
                    hb_status,
                    consecutive_hb_failures
                );
                let _ = log_event(
                    &log_file,
                    "warning",
                    "heartbeat.transient_failure",
                    json!({"status": hb_status.as_u16(), "consecutive": consecutive_hb_failures}),
                );
                if consecutive_hb_failures >= 5 {
                    let _ = log_event(
                        &log_file,
                        "error",
                        "heartbeat.max_failures_reached",
                        json!({}),
                    );
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(poll_seconds)) => {}
                    _ = shutdown_token.cancelled() => { break; }
                }
                continue;
            }
        }

        let hb_data: ApiResponse<ClientHeartbeatData> = serde_json::from_str(&hb_body)
            .with_context(|| {
                format!(
                    "heartbeat invalid json (status={}, body={})",
                    hb_status,
                    snippet(&hb_body)
                )
            })?;
        if !hb_data.success || hb_data.data.relogin_required {
            let _ = log_event(
                &log_file,
                "warning",
                "heartbeat.relogin_required",
                json!({
                    "alive": hb_data.data.alive,
                    "relogin_required": hb_data.data.relogin_required
                }),
            );
            let locale = crate::i18n::detect_cli_locale();
            eprintln!("{}", crate::i18n::t("cli.session_expired_relogin", &locale));
            let _ = log_event(
                &log_file,
                "warning",
                "auth.session_expired_attempting_relogin",
                json!({ "reason": "heartbeat_relogin_required" }),
            );
            match crate::oauth::cli_oauth_login(
                &client,
                &server_base,
                &device_id,
                &log_file,
                &db_path,
            )
            .await
            {
                Ok(new_token) => {
                    session_token = new_token;
                    consecutive_hb_failures = 0;
                    let _ = log_event(&log_file, "info", "auth.relogin_ok", json!({}));
                    continue;
                }
                Err(relogin_err) => {
                    eprintln!(
                        "{}: {}",
                        crate::i18n::t("cli.login_failed", &locale),
                        relogin_err
                    );
                    let _ = log_event(
                        &log_file,
                        "error",
                        "auth.relogin_failed",
                        json!({"error": format!("{:#}", relogin_err)}),
                    );
                    break;
                }
            }
        }

        // Heartbeat succeeded — reset failure counter
        consecutive_hb_failures = 0;

        // fetch domains
        let domains_url = format!("{}/api/v1/client/domains", server_base);
        let domains_raw: Value = match request_json_encrypted(
            client.get(domains_url).headers(headers.clone()),
            "client domains",
            &session_token,
        )
        .await
        {
            Ok(resp) => resp,
            Err(err) => {
                let err_text = format!("{:#}", err);
                if is_auth_error_message(&err_text) {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "domains.auth_expired",
                        json!({ "error": snippet(&err_text) }),
                    );
                    let locale = crate::i18n::detect_cli_locale();
                    eprintln!("{}", crate::i18n::t("cli.session_expired_relogin", &locale));
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "auth.session_expired_attempting_relogin",
                        json!({ "reason": "domains_unauthorized" }),
                    );
                    match crate::oauth::cli_oauth_login(
                        &client,
                        &server_base,
                        &device_id,
                        &log_file,
                        &db_path,
                    )
                    .await
                    {
                        Ok(new_token) => {
                            session_token = new_token;
                            let _ = log_event(&log_file, "info", "auth.relogin_ok", json!({}));
                            continue;
                        }
                        Err(relogin_err) => {
                            eprintln!(
                                "{}: {}",
                                crate::i18n::t("cli.login_failed", &locale),
                                relogin_err
                            );
                            let _ = log_event(
                                &log_file,
                                "error",
                                "auth.relogin_failed",
                                json!({"error": format!("{:#}", relogin_err)}),
                            );
                            break;
                        }
                    }
                } else {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "domains.fetch_transient_failure",
                        json!({"error": snippet(&err_text)}),
                    );
                    eprintln!(
                        "{}: {}",
                        crate::i18n::t(
                            "cli.domains_fetch_failed",
                            &crate::i18n::detect_cli_locale()
                        ),
                        snippet(&err_text)
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(poll_seconds)) => {}
                        _ = shutdown_token.cancelled() => { break; }
                    }
                    continue;
                }
            }
        };
        let domains_resp = parse_domains_data_value(domains_raw)?;
        let _ = log_event(
            &log_file,
            "info",
            "domains.synced",
            json!({ "count": domains_resp.items.len() }),
        );

        let task_type_bindings_arc = Arc::new(task_type_component_bindings.clone());
        let rule_bindings_arc = Arc::new(rule_component_bindings.clone());
        let governor = Arc::new(crate::resource_governor::ResourceGovernor::from_env());
        let _ = log_event(
            &log_file,
            "info",
            "worker.governor_initialized",
            json!({
                "domain_concurrency": governor.domain_concurrency,
                "global_translation_concurrency": governor.global_translation_concurrency,
                "global_callback_concurrency": governor.global_callback_concurrency,
            }),
        );

        // Prepare domain tasks: resolve tokens/secrets before spawning
        // to keep the spawn closures simpler.
        struct DomainTask {
            api_base_url: String,
            wp_client_token: String,
            route_secret: Option<String>,
            wp_base: String,
            domain_base: String,
            /// Identity Contract v1.1 §5 (C-1): verified identity this domain
            /// dispatches under; checked again at the discoverer lane entry.
            binding_identity: crate::types::PluginIdentity,
            is_local_dev: bool,
            relation_limit: Option<usize>,
        }
        let mut domain_tasks: Vec<DomainTask> = Vec::new();
        let mut missing_route_secret_domains: Vec<String> = Vec::new();
        let mut identity_bindings_dirty = false;
        let occupied_domain_bases: HashSet<String> = domains_resp
            .items
            .iter()
            .filter(|d| d.site_status != LOCAL_DEV_LICENSE_STATUS)
            .map(|d| normalize_domain_base(&d.api_base_url))
            .filter(|v| !v.is_empty())
            .collect();

        for domain in &domains_resp.items {
            let is_local_dev = domain.site_status == LOCAL_DEV_LICENSE_STATUS;
            if !is_local_dev && domain.site_status != "active" {
                let _ = log_event(
                    &log_file,
                    "info",
                    "domain.non_active_license",
                    json!({
                        "api_base_url": domain.api_base_url,
                        "site_status": domain.site_status
                    }),
                );
            }
            let (domain_api_base, domain_wp_client_token, domain_route_secret) = if is_local_dev {
                if let Some((local_base, token, secret)) =
                    resolve_local_dev_binding(&domain_token_bindings, &occupied_domain_bases)
                {
                    (local_base, token, secret)
                } else {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "domain.local_dev_missing_binding",
                        json!({
                            "slot_api_base_url": domain.api_base_url
                        }),
                    );
                    continue;
                }
            } else {
                let Some(token) = resolve_wp_client_token_for_domain(
                    &domain.api_base_url,
                    &domain_token_bindings,
                    &wp_client_token_fallback,
                ) else {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "domain.skipped_missing_wp_client_token",
                        json!({
                            "api_base_url": domain.api_base_url
                        }),
                    );
                    continue;
                };
                let route_secret =
                    resolve_route_secret_for_domain(&domain.api_base_url, &domain_token_bindings)
                        .or_else(|| {
                            domain
                                .route_secret
                                .as_deref()
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string())
                        });
                (domain.api_base_url.clone(), token, route_secret)
            };

            let domain_base = normalize_domain_base(&domain_api_base);
            let relation_limit = if is_local_dev {
                Some(1)
            } else {
                domain.max_relations.and_then(|limit| {
                    if limit > 0 {
                        Some(limit as usize)
                    } else {
                        None
                    }
                })
            };
            let wp_base = if is_local_dev {
                domain_route_secret
                    .as_deref()
                    .and_then(|s| build_wp_base_url(&domain_base, s))
                    .unwrap_or_else(|| domain_api_base.clone())
            } else {
                let Some(wp_base) = domain_route_secret
                    .as_deref()
                    .and_then(|s| build_wp_base_url(&domain_base, s))
                else {
                    missing_route_secret_domains.push(domain.api_base_url.clone());
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "domain.skipped_missing_route_secret",
                        json!({
                            "api_base_url": domain.api_base_url
                        }),
                    );
                    continue;
                };
                wp_base
            };

            // Identity Contract v1.1 §5 (C-1): same fail-closed gate as the
            // local-worker loop — unknown / mismatched / stale identity never
            // dispatches. No credential fields in events.
            let Some(entry) = resolve_entry_for_domain(&domain_api_base, &domain_token_bindings)
            else {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "identity.no_binding_entry",
                    json!({ "api_base_url": domain_api_base }),
                );
                continue;
            };
            let binding_identity = match gate(entry, now_unix()) {
                GateVerdict::Fresh(identity) => Some(identity),
                GateVerdict::Reverify { .. } => {
                    // Identity-aware verify base (contract §2/§3), same as
                    // the local-worker loop above.
                    let expected_identity = entry
                        .plugin_identity
                        .unwrap_or(crate::types::PluginIdentity::WpmmccAts);
                    let verify_base = build_verify_base_url(
                        &domain_base,
                        domain_route_secret.as_deref().unwrap_or(""),
                        &expected_identity,
                    )
                    .unwrap_or_else(|| wp_base.clone());
                    let outcome = verify_identity(
                        &client,
                        &verify_base,
                        &domain_wp_client_token,
                        &worker_config.worker_id,
                        &worker_config.device_id,
                        domain_route_secret.as_deref(),
                    )
                    .await;
                    match classify_verify_outcome(&outcome, entry) {
                        Ok(verified) => {
                            let caps = match &outcome {
                                VerifyOutcome::Verified(_, caps) => caps.clone(),
                                _ => crate::types::IdentityCapabilities::default(),
                            };
                            let verified_at = format_rfc3339_utc(now_unix());
                            let legacy_key = normalize_api_base_url_key(&domain_api_base);
                            let stored = match domain_token_bindings.domains.get_mut(&domain_base) {
                                Some(stored) => Some(stored),
                                None => domain_token_bindings.domains.get_mut(&legacy_key),
                            };
                            if let Some(stored) = stored {
                                stored.plugin_identity = Some(verified);
                                stored.identity_verified_at = Some(verified_at);
                                stored.identity_capabilities = Some(caps);
                                identity_bindings_dirty = true;
                            }
                            Some(verified)
                        }
                        Err(Some(code)) => {
                            let _ = log_event(
                                &log_file,
                                "warning",
                                code,
                                json!({ "api_base_url": domain_api_base }),
                            );
                            None
                        }
                        Err(None) => {
                            let _ = log_event(
                                &log_file,
                                "warning",
                                "identity.verify_failed",
                                json!({ "api_base_url": domain_api_base }),
                            );
                            None
                        }
                    }
                }
            };
            let Some(binding_identity) = binding_identity else {
                continue;
            };

            // FL-3 (Wave-2): same ATS-lane pre-filter as the local-worker
            // collector — wpmmcc-bound domains belong to the pair engine,
            // never to this discovery lane.
            if binding_identity != crate::types::PluginIdentity::WpmmccAts {
                if crate::bindings::identity_exclusion_first_notice(
                    "wpmmcc_ats_task_generation",
                    &domain_api_base,
                    binding_identity.as_wire_str(),
                ) {
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.domain_lane_excluded",
                        json!({
                            "api_base_url": domain_api_base,
                            "lane": "wpmmcc_ats",
                            "binding_identity": binding_identity.as_wire_str(),
                        }),
                    );
                }
                continue;
            }

            domain_tasks.push(DomainTask {
                api_base_url: domain_api_base,
                wp_client_token: domain_wp_client_token,
                route_secret: domain_route_secret,
                wp_base,
                domain_base,
                binding_identity,
                is_local_dev,
                relation_limit,
            });
        }

        if identity_bindings_dirty {
            if let Err(err) =
                save_domain_token_bindings(&domain_token_bindings_path, &domain_token_bindings)
            {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "worker.identity_bindings_save_failed",
                    json!({
                        "path": domain_token_bindings_path,
                        "error": format!("{:#}", err)
                    }),
                );
            }
        }

        // Spawn concurrent domain workers, limited by ResourceGovernor.
        let mut domain_joins: tokio::task::JoinSet<anyhow::Result<Option<DomainRunReport>>> =
            tokio::task::JoinSet::new();

        for dt in domain_tasks {
            let client = client.clone();
            let server_base = server_base.clone();
            let session_token = session_token.clone();
            let log_file = log_file.clone();
            let db_path = db_path.clone();
            let component_registry = component_registry.clone();
            let proxy_pool = proxy_pool.clone();
            let component_id_override = component_id_override.clone();
            let component_prefer_ids = component_prefer_ids.clone();
            let task_type_bindings = Arc::clone(&task_type_bindings_arc);
            let rule_bindings = Arc::clone(&rule_bindings_arc);
            let worker_config = worker_config.clone();
            let pending_callback_store = Arc::clone(&pending_callback_store);
            let governor = Arc::clone(&governor);

            let bindings_path_for_recovery = domain_token_bindings_path.clone();
            let bindings_doc_for_recovery = domain_token_bindings.clone();
            let domain_lease = Arc::clone(&runtime_lease);
            let db = Arc::clone(&db);
            domain_joins.spawn(async move {
                    let _lease = domain_lease;
                    // Acquire domain concurrency permit
                    let Some(_domain_permit) = governor.domain_sem.acquire().await.ok() else {
                        return Ok(None);
                    };

                    // Track effective endpoint/secret for retries in this domain run.
                    let mut run_wp_base = dt.wp_base.clone();
                    let mut run_route_secret = dt.route_secret.clone();

                    let mut result = discover_and_translate(
                        &client,
                        &run_wp_base,
                        &dt.wp_client_token,
                        &log_file,
                        component_registry.clone(),
                        proxy_pool.clone(),
                        &component_id_override,
                        &component_prefer_ids,
                        Some(task_type_bindings.clone()),
                        Some(rule_bindings.as_ref()),
                        &worker_config,
                        run_route_secret.as_deref(),
                        &pending_callback_store,
                        Some(Arc::clone(&governor.global_translation_sem)),
                        Some(Arc::clone(&governor.global_callback_sem)),
                        dt.relation_limit,
                        Some(Arc::clone(&db)),
                        dt.binding_identity,
                    )
                    .await;

                    // On 404, try refreshing route_secret from server and retry once.
                    if !dt.is_local_dev
                        && result
                        .as_ref()
                        .err()
                        .map(|err| format!("{:#}", err).contains("status=404"))
                        .unwrap_or(false)
                    {
                        let _ = log_event(
                            &log_file,
                            "info",
                            "domain.404_refreshing_route_secret",
                            json!({ "api_base_url": dt.api_base_url }),
                        );
                        let refresh_url = format!("{}/api/v1/client/domains", server_base);
                        let mut refresh_headers = HeaderMap::new();
                        if let Ok(hv) = session_token.parse() {
                            refresh_headers.insert("X-Client-Session", hv);
                        }
                        if let Ok(hv) = CLIENT_MODE_DESKTOP.parse() {
                            refresh_headers.insert(CLIENT_MODE_HEADER, hv);
                        }
                        let refreshed_raw = request_json_encrypted::<Value>(
                            client.get(&refresh_url).headers(refresh_headers),
                            "refresh domains (404 recovery)",
                            &session_token,
                        )
                        .await?;
                        let refreshed_domains = parse_domains_data_value(refreshed_raw)?;
                        if let Some(updated_domain) = refreshed_domains
                            .items
                            .iter()
                            .find(|d| normalize_domain_base(&d.api_base_url) == dt.domain_base)
                        {
                            let new_secret = updated_domain
                                .route_secret
                                .as_deref()
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string());
                            if new_secret != run_route_secret {
                                let _ = log_event(
                                    &log_file,
                                    "info",
                                    "domain.route_secret_refreshed",
                                    json!({
                                        "api_base_url": dt.api_base_url,
                                        "had_user_binding": dt.wp_base != dt.api_base_url,
                                    }),
                                );
                                let Some(new_wp_base) = new_secret
                                    .as_deref()
                                    .and_then(|s| build_wp_base_url(&dt.domain_base, s))
                                else {
                                    return Err(anyhow::anyhow!(
                                        "MISSING_ROUTE_SECRET: route_secret is missing for active domain {} after 404 recovery",
                                        dt.domain_base
                                    ));
                                };
                                run_wp_base = new_wp_base;
                                run_route_secret = new_secret.clone();

                                result = discover_and_translate(
                                    &client,
                                    &run_wp_base,
                                    &dt.wp_client_token,
                                    &log_file,
                                    component_registry.clone(),
                                    proxy_pool.clone(),
                                    &component_id_override,
                                    &component_prefer_ids,
                                    Some(task_type_bindings.clone()),
                                    Some(rule_bindings.as_ref()),
                                    &worker_config,
                                    run_route_secret.as_deref(),
                                    &pending_callback_store,
                                    Some(Arc::clone(&governor.global_translation_sem)),
                                    Some(Arc::clone(&governor.global_callback_sem)),
                                    dt.relation_limit,
                                    Some(Arc::clone(&db)),
                                    dt.binding_identity,
                                )
                                .await;

                                // C3: Persist refreshed route_secret to DB on successful retry
                                if result.is_ok() {
                                    if let Some(ref new_s) = run_route_secret {
                                        if let Ok(conn) = crate::db::open_db(&db_path) {
                                            match crate::db::bindings::persist_refreshed_route_secret(
                                                &conn, &dt.domain_base, &dt.api_base_url,
                                                &dt.wp_client_token, new_s,
                                            ) {
                                                Ok(doc) => {
                                                    if crate::bindings::save_domain_token_bindings(
                                                        &bindings_path_for_recovery, &doc,
                                                    ).is_err() {
                                                        let _ = log_event(&log_file, "warning", "domain.route_secret_mirror_failed",
                                                            json!({"domain": dt.domain_base}));
                                                    }
                                                    let _ = log_event(&log_file, "info", "domain.route_secret_persisted",
                                                        json!({"domain": dt.domain_base}));
                                                }
                                                Err(_) => {
                                                    let _ = log_event(&log_file, "warning", "domain.route_secret_store_failed",
                                                        json!({"domain": dt.domain_base}));
                                                }
                                            }
                                        } else {
                                            let _ = log_event(&log_file, "warning", "domain.route_secret_store_failed",
                                                json!({"domain": dt.domain_base}));
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // On WP 401 (token rotation / signature mismatch), retry with exponential backoff (60s/120s/240s)
                    if result
                        .as_ref()
                        .err()
                        .map(|err| is_wp_token_rotation_error(&format!("{:#}", err)))
                        .unwrap_or(false)
                    {
                        let _ = log_event(
                            &log_file,
                            "warning",
                            "domain.wp_token_rotation_detected",
                            json!({ "api_base_url": dt.api_base_url }),
                        );
                        let backoff_secs = [60u64, 120, 240];
                        for (attempt, &wait) in backoff_secs.iter().enumerate() {
                            let _ = log_event(
                                &log_file,
                                "info",
                                "domain.wp_token_rotation_retry",
                                json!({
                                    "api_base_url": dt.api_base_url,
                                    "attempt": attempt + 1,
                                    "wait_seconds": wait,
                                }),
                            );
                            tokio::time::sleep(Duration::from_secs(wait)).await;
                            let refreshed_bindings =
                                load_domain_token_bindings(&bindings_path_for_recovery)
                                    .unwrap_or_else(|_| bindings_doc_for_recovery.clone());
                            let fallback_wp_client_token = env_or("WPTSALL_WP_CLIENT_TOKEN", "");
                            let refreshed_token = resolve_wp_client_token_for_domain(
                                &dt.api_base_url,
                                &refreshed_bindings,
                                &fallback_wp_client_token,
                            )
                            .unwrap_or_else(|| dt.wp_client_token.clone());
                            let retry_result = discover_and_translate(
                                &client,
                                &run_wp_base,
                                &refreshed_token,
                                &log_file,
                                component_registry.clone(),
                                proxy_pool.clone(),
                                &component_id_override,
                                &component_prefer_ids,
                                Some(task_type_bindings.clone()),
                                Some(rule_bindings.as_ref()),
                                &worker_config,
                                run_route_secret.as_deref(),
                                &pending_callback_store,
                                Some(Arc::clone(&governor.global_translation_sem)),
                                Some(Arc::clone(&governor.global_callback_sem)),
                                dt.relation_limit,
                                Some(Arc::clone(&db)),
                                dt.binding_identity,
                            )
                            .await;
                            let should_stop = retry_result.is_ok()
                                || retry_result
                                    .as_ref()
                                    .err()
                                    .map(|err| !is_wp_token_rotation_error(&format!("{:#}", err)))
                                    .unwrap_or(true);
                            result = retry_result;
                            if should_stop {
                                break;
                            }
                        }
                        if result
                            .as_ref()
                            .err()
                            .map(|err| is_wp_token_rotation_error(&format!("{:#}", err)))
                            .unwrap_or(false)
                        {
                            let _ = log_event(
                                &log_file,
                                "warning",
                                "domain.token_stale_skipped",
                                json!({ "api_base_url": dt.api_base_url }),
                            );
                        }
                    }

                    match result {
                        Ok(report) => Ok(Some(report)),
                        Err(err) => {
                            let err_text = format!("{:#}", err);
                            let is_not_found = err_text.contains("status=404");
                            let level = if is_not_found { "warning" } else { "error" };
                            let _ = log_event(
                                &log_file,
                                level,
                                "domain.process_failed",
                                json!({
                                    "api_base_url": dt.api_base_url,
                                    "error": err_text
                                }),
                            );
                            let cli_locale = crate::i18n::detect_cli_locale();
                            if is_not_found {
                                let msg = crate::i18n::t("cli.process_domain_warning", &cli_locale);
                                eprintln!("{}", msg.replacen("{}", &dt.api_base_url, 1).replacen("{}", &err.to_string(), 1));
                            } else {
                                let msg = crate::i18n::t("cli.process_domain_failed", &cli_locale);
                                eprintln!("{}", msg.replacen("{}", &dt.api_base_url, 1).replacen("{}", &err.to_string(), 1));
                            }
                            if is_fatal_control_plane_error(&err_text) {
                                Err(err)
                            } else {
                                Ok(None)
                            }
                        }
                    }
                });
        }

        // Collect results from all domain workers (cancellation-aware: 30s grace)
        let mut loop_reports: Vec<DomainRunReport> = Vec::new();
        let mut fatal_domain_error: Option<anyhow::Error> = None;
        if shutdown_token.is_cancelled() {
            // Give in-flight tasks 30s to finish
            let grace = tokio::time::timeout(Duration::from_secs(30), async {
                while let Some(join_result) = domain_joins.join_next().await {
                    match join_result {
                        Ok(Ok(Some(report))) => loop_reports.push(report),
                        Ok(Ok(None)) => {}
                        Ok(Err(err)) => {
                            if fatal_domain_error.is_none() {
                                fatal_domain_error = Some(err);
                            }
                        }
                        Err(err) => {
                            if fatal_domain_error.is_none() {
                                fatal_domain_error = Some(anyhow::anyhow!(
                                    "worker domain task join failed: {}",
                                    err
                                ));
                            }
                        }
                    }
                }
            });
            let _ = grace.await;
            domain_joins.abort_all();
        } else {
            while let Some(join_result) = domain_joins.join_next().await {
                match join_result {
                    Ok(Ok(Some(report))) => loop_reports.push(report),
                    Ok(Ok(None)) => {}
                    Ok(Err(err)) => {
                        if fatal_domain_error.is_none() {
                            fatal_domain_error = Some(err);
                        }
                    }
                    Err(err) => {
                        if fatal_domain_error.is_none() {
                            fatal_domain_error =
                                Some(anyhow::anyhow!("worker domain task join failed: {}", err));
                        }
                    }
                }
            }
        }

        if let Some(err) = fatal_domain_error {
            return Err(err);
        }

        if !loop_reports.is_empty() {
            log_loop_summary(&log_file, &loop_reports);
        }
        if !missing_route_secret_domains.is_empty() {
            let _ = log_event(
                &log_file,
                "warning",
                "worker.domains_skipped_missing_route_secret",
                json!({
                    "count": missing_route_secret_domains.len(),
                    "domains": missing_route_secret_domains,
                }),
            );
        }

        if one_shot || shutdown_token.is_cancelled() {
            break;
        }

        // Cancellation-aware sleep
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(poll_seconds)) => {}
            _ = shutdown_token.cancelled() => {
                let _ = log_event(&log_file, "info", "worker.shutdown_during_sleep", json!({}));
                break;
            }
        }
    }

    // Logout: best-effort session cleanup
    let logout_url = format!("{}/api/v1/client/logout", server_base);
    match client
        .post(&logout_url)
        .header("X-Client-Session", &session_token)
        .header(REQUEST_ID_HEADER, build_request_id("logout"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let _ = log_event(&log_file, "info", "auth.logout_ok", json!({}));
        }
        Ok(resp) => {
            let _ = log_event(
                &log_file,
                "warning",
                "auth.logout_failed",
                json!({ "status": resp.status().as_u16() }),
            );
        }
        Err(err) => {
            let _ = log_event(
                &log_file,
                "warning",
                "auth.logout_failed",
                json!({ "error": format!("{:#}", err) }),
            );
        }
    }

    maybe_export_log(&log_file, &log_export_path)?;
    let _ = log_event(&log_file, "info", "shutdown", json!({}));
    crate::logging::flush_log();

    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn log_loop_summary(log_file: &str, loop_reports: &[DomainRunReport]) {
    let domain_count = loop_reports.len();
    let total_pulled: usize = loop_reports.iter().map(|r| r.pulled).sum();
    let total_processed: usize = loop_reports.iter().map(|r| r.processed).sum();
    let total_completed: usize = loop_reports.iter().map(|r| r.completed).sum();
    let total_retried: usize = loop_reports.iter().map(|r| r.retried).sum();
    let total_failed: usize = loop_reports.iter().map(|r| r.failed).sum();
    let total_status_update_failed: usize =
        loop_reports.iter().map(|r| r.status_update_failed).sum();
    let avg_elapsed_ms = if domain_count == 0 {
        0
    } else {
        let total: u64 = loop_reports.iter().map(|r| r.avg_elapsed_ms).sum();
        total / u64::try_from(domain_count).unwrap_or(1)
    };
    let _ = log_event(
        log_file,
        "info",
        "loop.queue_summary",
        json!({
            "domain_count": domain_count,
            "pulled": total_pulled,
            "processed": total_processed,
            "completed": total_completed,
            "retried": total_retried,
            "failed": total_failed,
            "status_update_failed": total_status_update_failed,
            "avg_elapsed_ms": avg_elapsed_ms,
            "domains": loop_reports
                .iter()
                .map(|r| json!({
                    "api_base_url": r.api_base_url,
                    "trace_id": r.trace_id,
                    "pulled": r.pulled,
                    "processed": r.processed,
                    "completed": r.completed,
                    "retried": r.retried,
                    "failed": r.failed,
                    "status_update_failed": r.status_update_failed,
                    "avg_elapsed_ms": r.avg_elapsed_ms
                }))
                .collect::<Vec<Value>>()
        }),
    );
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_worker_config_defaults() {
        let config = build_worker_config("worker-test-default");
        assert_eq!(config.worker_id, "worker-test-default");
        // opus5 A-03 (AF-03): the boot-level device identity rides on the
        // config; WPTSALL_WORKER_ID may only relabel the lane, never change
        // the paired device identity.
        assert_eq!(config.device_id, "worker-test-default");
        assert_eq!(config.task_pull_statuses, vec!["pending", "retry"]);
        assert!(config.task_concurrency >= 1);
        assert_eq!(config.retry_max, 2);
        assert!(config.discovery_mode);
        assert!(!config.review_mode);
    }

    #[test]
    fn test_fatal_control_plane_error_classification() {
        assert!(is_fatal_control_plane_error(
            "Upstream returned RATE_LIMITED error"
        ));
        assert!(is_fatal_control_plane_error(
            "unauthorized: session expired"
        ));
        assert!(is_fatal_control_plane_error("status=401"));
        assert!(!is_fatal_control_plane_error("connection reset by peer"));
    }
}