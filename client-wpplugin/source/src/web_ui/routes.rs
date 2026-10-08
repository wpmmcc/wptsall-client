use serde_json::{json, Value};
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use anyhow::Context;

// P1-8b: the `account` module (server-control-plane profile/ip-whitelist/
// locale helpers hitting `server_base/api/v1/client/*`) was fully dead —
// zero `account::` references anywhere in src/frontend/desktop — so the
// whole module and its `mod account;` declaration were removed. Local-first
// status does not need any of those server-backed account surfaces.
mod auth;
mod bindings;
mod components;
mod errors;
mod http;
mod integrations;
pub(crate) mod legacy_routes;
mod media_recovery;
mod operations;
mod platform;
mod provider_catalog;
mod provider_recovery;
mod providers;
mod review;
mod settings;
mod storage;
mod sync_pairs;
mod sync_review;
mod worker;

pub(crate) use self::worker::stop_worker_loop;

use self::sync_pairs::{
    handle_sync_pair_credential_delete, handle_sync_pair_credentials_list,
    handle_sync_pairs_delete, handle_sync_pairs_list, handle_sync_pairs_pair,
    handle_sync_pairs_pause, handle_sync_pairs_resume, handle_sync_pairs_run,
    handle_sync_pairs_upsert, SyncPairDeleteRequest,
};
use self::sync_review::{
    handle_sync_review_approve, handle_sync_review_detail, handle_sync_review_list,
    handle_sync_review_reject, handle_sync_review_update,
};

use self::auth::{
    handle_domains_refresh, handle_logout, handle_oauth_callback, handle_oauth_start,
};
use self::bindings::{
    handle_bindings_delete, handle_bindings_upsert, handle_domain_tokens_delete,
    handle_domain_tokens_test, handle_domain_tokens_upsert, handle_rule_component_bindings_delete,
    handle_rule_component_bindings_upsert, handle_site_connections_import,
    handle_task_type_components_delete, handle_task_type_components_upsert,
};
#[cfg(test)]
use self::components::{
    backfill_local_components_from_server, invalid_task_override_paths,
    patch_template_snapshot_for_local_kind,
};
use self::components::{
    build_local_component_runtime_for_task, component_exists_in_local_doc,
    fetch_signing_key_from_server, find_server_component_by_template_id,
    handle_component_version_create, handle_component_version_delete,
    handle_component_version_test, handle_component_version_update, handle_components_capabilities,
    handle_components_refresh, handle_components_template, handle_install_server_template_to_local,
    handle_local_component_create_v2, handle_local_component_delete_v2,
    handle_local_component_detail, handle_local_component_export, handle_local_component_import,
    handle_local_component_quick_test, handle_local_component_refresh_snapshot,
    handle_local_component_test, handle_local_component_test_file,
    handle_local_component_update_v2, handle_local_components_list,
    handle_rule_component_binding_discovery, handle_server_components_search,
    is_signature_related_error, load_local_components_runtime_doc, local_component_capability_id,
    normalized_vendor_id, persist_signing_key_material, save_component_bindings_runtime_doc,
    save_domain_token_bindings_runtime_doc, save_local_components_runtime_doc,
    save_rule_component_bindings_runtime_doc, save_task_type_component_bindings_runtime_doc,
    sync_local_components_from_server, validate_component_binding_vendor_alignment,
    validate_editable_overrides_for_component_id, validate_local_component_runtime_ready_for_task,
    validate_server_component_api_version, validate_task_editable_overrides,
};
use self::errors::{
    err_public, maybe_write_upstream_api_error, write_error_response,
    write_error_response_with_status, write_session_required,
};
use self::http::{read_simple_http_request, write_html_page_response, write_http_response};
use self::integrations::{
    handle_oauth_authorize, handle_oauth_config_create, handle_oauth_config_delete,
    handle_oauth_config_update, handle_oauth_configs_list, handle_proxy_create,
    handle_proxy_delete, handle_proxy_list, handle_proxy_test, handle_proxy_update,
    handle_vendor_key_create, handle_vendor_key_delete, handle_vendor_key_update,
    handle_vendor_keys_list, handle_vendor_oauth_callback,
};
use self::operations::{
    handle_discovery_task_update, handle_discovery_tasks_bootstrap, handle_discovery_tasks_list,
    handle_logs_recent, handle_logs_recent_get, handle_stats_overview,
    handle_translations_batch_delete, handle_translations_batch_retry, handle_translations_list,
};
use self::platform::{handle_platform_entitlements, handle_platform_products};
use self::provider_catalog::{
    handle_install_from_catalog, handle_integration_pack_export, handle_integration_pack_import,
    handle_integration_pack_preview, handle_provider_catalog_install_version,
    handle_provider_catalog_list, handle_provider_catalog_refresh,
    handle_provider_catalog_versions,
};
use self::providers::handle_providers_test;
use self::review::{
    handle_item_approve, handle_item_content, handle_item_override_save, handle_item_reject,
    handle_item_resubmit, handle_item_retranslate, handle_item_translated_save,
    handle_items_batch_approve, handle_items_batch_reject, handle_items_pending_review,
    handle_job_detail, handle_job_items, handle_jobs_list,
};
use self::settings::{
    handle_access_control_get, handle_access_control_update, handle_log_settings_get,
    handle_log_settings_update, webui_access_token,
};
pub(crate) use self::worker::spawn_worker_loop;
use self::worker::{
    handle_worker_config, handle_worker_config_get, handle_worker_run_once, handle_worker_start,
    handle_worker_start_check, handle_worker_stop,
};

use crate::bindings::{
    domain_token_binding_status_items, normalize_domain_base, rule_component_binding_status_items,
    task_type_component_binding_status_items,
};
#[cfg(test)]
use crate::bindings::{load_components_local, save_components_local};
use crate::logging::{session_token_prefix, unix_ts};
use crate::types::*;
use crate::web_ui::{
    fetch_cloud_api_types_for_session_with_signing_key, fetch_domains_for_session,
    fetch_vendors_for_session, fetch_wp_translation_providers_for_session_with_signing_key,
    read_trusted_signing_key_pem_from_state,
    static_html::{web_ui_favicon_ico, web_ui_favicon_svg, web_ui_html},
};

pub(crate) async fn handle_web_ui_connection(
    mut socket: TcpStream,
    state: Arc<Mutex<WebUiState>>,
    runtime_control: WebUiRuntimeControl,
    log_file: &str,
    _started_at_epoch: u64,
    start_time: std::time::Instant,
    access_control: super::AccessControl,
) -> anyhow::Result<()> {
    let Some((method, target, query, headers, body)) =
        read_simple_http_request(&mut socket).await?
    else {
        return Ok(());
    };

    // Every state-changing or unknown method must pass the Origin gate,
    // including methods added to the dispatcher in the future.
    if !matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS") {
        let origin_ok =
            check_csrf_origin(headers.get("origin").map(|s| s.as_str()), &access_control).await;
        if !origin_ok {
            return write_error_response_with_status(
                &mut socket,
                "403 Forbidden",
                "CSRF_REJECTED",
                "cross-origin request blocked",
            )
            .await;
        }
    }

    // S4 (07 号 audit): validate the Host header against an allowlist
    // before any handler runs. DNS-rebinding attacks reach the loopback
    // listener with the attacker's domain in Host; browsers and HTTP/1.1
    // clients always send an expected Host, so a missing or unexpected
    // Host is rejected (fail-closed).
    if !check_host_header(headers.get("host").map(|s| s.as_str()), &access_control).await {
        return write_error_response_with_status(
            &mut socket,
            "403 Forbidden",
            "WEBUI_HOST_REJECTED",
            "host header not allowed",
        )
        .await;
    }

    // S2 (07 号 audit): in external mode every /api request must present
    // the WebUI access token (`X-WPTSALL-WebUI-Token`). Loopback peers
    // keep read access to GET /api/access-control so the token can be
    // recovered on the machine itself; static assets stay open (they are
    // the token entry surface and hold no state). Local loopback mode is
    // exempt — the loopback bind, the Origin check above and the Host
    // allowlist already gate it.
    let peer_ip = socket.peer_addr().map(|addr| addr.ip()).ok();
    if webui_token_required(
        access_control.is_external().await,
        peer_ip,
        &method,
        &target,
    ) {
        let provided = headers
            .get("x-wptsall-webui-token")
            .map(|s| s.trim())
            .unwrap_or("");
        let expected = webui_access_token(&state).await?;
        if !constant_time_eq(expected.as_bytes(), provided.as_bytes()) {
            return write_error_response_with_status(
                &mut socket,
                "401 Unauthorized",
                "WEBUI_TOKEN_REQUIRED",
                "web ui access token required",
            )
            .await;
        }
    }

    // P0-LF-02: reject legacy website/control-plane routes BEFORE any handler
    // can run — no HTTP client clones, no session reads, no `server_base` URL
    // building. The classifier is the single source of truth; the
    // route-classification test fails if a legacy handler becomes reachable
    // without being listed there.
    if legacy_routes::legacy_route_blocked(&method, &target) {
        return write_error_response_with_status(
            &mut socket,
            legacy_routes::LEGACY_DISABLED_STATUS,
            legacy_routes::LEGACY_DISABLED_CODE,
            legacy_routes::LEGACY_DISABLED_MESSAGE,
        )
        .await;
    }

    match (method.as_str(), target.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            let html = web_ui_html();
            write_html_page_response(&mut socket, &html).await?;
        }
        ("GET", "/favicon.svg") | ("GET", "/logo.svg") => {
            write_http_response(&mut socket, "200 OK", "image/svg+xml", web_ui_favicon_svg())
                .await?;
        }
        ("GET", "/favicon.ico") => {
            write_http_response(&mut socket, "200 OK", "image/x-icon", web_ui_favicon_ico())
                .await?;
        }
        ("GET", "/health") => {
            handle_health(&mut socket, start_time).await?;
        }
        ("GET", "/api/status") => {
            handle_get_status(&mut socket, &state).await?;
        }
        ("GET", "/api/update-check") => {
            handle_update_check(&mut socket, &state).await?;
        }
        ("POST", "/api/perform-update") => {
            handle_perform_update(&mut socket, &state).await?;
        }
        ("POST", "/api/components/refresh") => {
            handle_components_refresh(&mut socket, &state).await?;
        }
        ("GET", "/api/components/capabilities") => {
            handle_components_capabilities(&mut socket, &state).await?;
        }
        ("POST", "/api/components/template") => {
            handle_components_template(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/components/test") => {
            handle_local_component_test(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/components/bindings/upsert") => {
            handle_bindings_upsert(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/components/bindings/delete") => {
            handle_bindings_delete(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/task-type-components/upsert") => {
            handle_task_type_components_upsert(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/task-type-components/delete") => {
            handle_task_type_components_delete(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/rule-component-bindings/upsert") => {
            handle_rule_component_bindings_upsert(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/rule-component-bindings/delete") => {
            handle_rule_component_bindings_delete(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/rule-component-bindings/discovery") => {
            handle_rule_component_binding_discovery(&mut socket, &state).await?;
        }
        ("POST", "/api/domain-tokens/upsert") => {
            handle_domain_tokens_upsert(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/domain-tokens/delete") => {
            handle_domain_tokens_delete(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/domain-tokens/test") => {
            handle_domain_tokens_test(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/site-connections/import") => {
            handle_site_connections_import(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/sync-pairs") => {
            handle_sync_pairs_list(&mut socket, &state).await?;
        }
        ("POST", "/api/sync-pairs") => {
            handle_sync_pairs_upsert(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/sync-pairs/delete") => {
            let req: SyncPairDeleteRequest = serde_json::from_slice(&body)?;
            handle_sync_pairs_delete(&mut socket, &state, &req.id).await?;
        }
        ("POST", "/api/worker/run-once") => {
            handle_worker_run_once(&mut socket, &state, log_file, &body).await?;
        }
        ("GET", "/api/media-recovery") => {
            media_recovery::list(&mut socket).await?;
        }
        ("POST", "/api/media-recovery/reconcile") => {
            media_recovery::reconcile(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/provider-recovery") => {
            provider_recovery::list(&mut socket, &state).await?;
        }
        ("POST", "/api/provider-recovery/reconcile") => {
            provider_recovery::reconcile(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/worker/start") => {
            handle_worker_start(&mut socket, &state, &runtime_control, log_file, &body).await?;
        }
        ("POST", "/api/worker/start-check") => {
            handle_worker_start_check(&mut socket, &state, log_file).await?;
        }
        ("GET", "/api/worker/config") => {
            handle_worker_config_get(&mut socket, &state).await?;
        }
        ("GET", "/api/storage/capacity") => {
            storage::get(&mut socket).await?;
        }
        ("POST", "/api/storage/capacity") => {
            storage::update(&mut socket, &body).await?;
        }
        ("POST", "/api/worker/config") => {
            handle_worker_config(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/worker/stop") => {
            handle_worker_stop(&mut socket, &state, log_file, &runtime_control).await?;
        }
        ("POST", "/api/logs/recent") => {
            handle_logs_recent(&mut socket, &state, &body, log_file).await?;
        }
        ("POST", "/api/logs/clear") => {
            handle_logs_clear(&mut socket, log_file).await?;
        }
        // Client-side (browser / desktop WebView) JS error ingestion: the
        // frontends report window.onerror + unhandledrejection here so
        // render crashes surface in the same log stream as engine events.
        ("POST", "/api/logs/client") => {
            handle_logs_client(&mut socket, &body).await?;
        }
        ("GET", "/api/logs/recent") => {
            handle_logs_recent_get(&mut socket, &state, &query, log_file).await?;
        }
        // Alias used by some desktop/docs clients.
        ("GET", "/api/logs") => {
            handle_logs_recent_get(&mut socket, &state, &query, log_file).await?;
        }
        ("POST", "/api/domains/refresh") => {
            handle_domains_refresh(&mut socket, &state).await?;
        }
        ("POST", "/api/components/local/test-file") => {
            handle_local_component_test_file(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/logout") => {
            handle_logout(&mut socket, &state, &runtime_control).await?;
        }
        ("POST", "/api/oauth/start") => {
            handle_oauth_start(&mut socket, &state).await?;
        }
        ("GET", "/oauth/callback") => {
            handle_oauth_callback(&mut socket, &state, &query, log_file).await?;
        }
        ("GET", "/oauth/vendor/callback") => {
            handle_vendor_oauth_callback(&mut socket, &state, &query).await?;
        }
        // --- New vendor/component/key/oauth/proxy endpoints ---
        // --- Platform endpoints ---
        ("GET", "/api/platform/products") => {
            handle_platform_products(&mut socket, &state, &query).await?;
        }
        ("GET", "/api/platform/entitlements") => {
            handle_platform_entitlements(&mut socket, &state, &query).await?;
        }
        ("GET", "/api/vendors") => {
            handle_vendors_list(&mut socket, &state).await?;
        }
        ("GET", "/api/wp-translation-providers") => {
            handle_wp_translation_providers_list(&mut socket, &state).await?;
        }
        ("GET", "/api/cloud-api-types") => {
            handle_cloud_api_types_list(&mut socket, &state).await?;
        }
        ("GET", "/api/components/local") => {
            handle_local_components_list(&mut socket, &state, &query).await?;
        }
        ("GET", "/api/provider-catalog") => {
            handle_provider_catalog_list(&mut socket, &state, &query).await?;
        }
        ("POST", "/api/provider-catalog/refresh") => {
            handle_provider_catalog_refresh(&mut socket, &state).await?;
        }
        ("GET", "/api/provider-catalog/versions") => {
            handle_provider_catalog_versions(&mut socket, &state).await?;
        }
        ("POST", "/api/provider-catalog/install-version") => {
            handle_provider_catalog_install_version(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/providers/test") => {
            handle_providers_test(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/components/server-search") => {
            handle_server_components_search(&mut socket, &state, &query).await?;
        }
        ("POST", "/api/components/local") => {
            handle_local_component_create_v2(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/components/local/import") => {
            handle_local_component_import(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/components/local/install-from-catalog") => {
            handle_install_from_catalog(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/components/local/install-from-server") => {
            handle_install_server_template_to_local(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/vendor-keys") => {
            handle_vendor_keys_list(&mut socket, &state, &query).await?;
        }
        ("POST", "/api/vendor-keys") => {
            handle_vendor_key_create(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/vendor-oauth") => {
            handle_oauth_configs_list(&mut socket, &state, &query).await?;
        }
        ("POST", "/api/vendor-oauth") => {
            handle_oauth_config_create(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/proxy-profiles") => {
            handle_proxy_list(&mut socket, &state).await?;
        }
        ("POST", "/api/proxy-profiles") => {
            handle_proxy_create(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/log-settings") => {
            handle_log_settings_get(&mut socket, &state).await?;
        }
        ("POST", "/api/log-settings") => {
            handle_log_settings_update(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/integrations/pack/export") => {
            handle_integration_pack_export(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/integrations/pack/preview") => {
            handle_integration_pack_preview(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/integrations/pack/import") => {
            handle_integration_pack_import(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/access-control") => {
            handle_access_control_get(&mut socket, &state, &access_control).await?;
        }
        ("POST", "/api/access-control") => {
            handle_access_control_update(&mut socket, &state, &access_control, &body).await?;
        }
        ("GET", "/api/translations") => {
            handle_translations_list(&mut socket, &state, &query).await?;
        }
        ("POST", "/api/translations/batch-retry") => {
            handle_translations_batch_retry(&mut socket, &state, &body).await?;
        }
        ("POST", "/api/translations/batch-delete") => {
            handle_translations_batch_delete(&mut socket, &state, &body).await?;
        }
        ("GET", "/api/jobs") => {
            handle_jobs_list(&mut socket, &state, &query).await?;
        }
        ("GET", "/api/discovery-tasks") => {
            handle_discovery_tasks_list(&mut socket, &state).await?;
        }
        ("POST", "/api/discovery-tasks/bootstrap") => {
            handle_discovery_tasks_bootstrap(&mut socket, &state).await?;
        }
        ("GET", "/api/stats/overview") => {
            handle_stats_overview(&mut socket, &state).await?;
        }
        _ => {
            // Dynamic routes
            let handled =
                handle_dynamic_routes(&mut socket, &state, &method, &target, &query, &body).await?;
            if !handled {
                let payload = json!({
                    "success": false,
                    "error": { "code": "NOT_FOUND", "message": "Not found" }
                });
                write_http_response(
                    &mut socket,
                    "404 Not Found",
                    "application/json",
                    &serde_json::to_vec(&payload)?,
                )
                .await?;
            }
        }
    }
    Ok(())
}

pub(super) async fn handle_capacity_only_connection(
    mut socket: TcpStream,
    start: std::time::Instant,
    access: super::AccessControl,
) -> anyhow::Result<()> {
    let Some((method, target, _, headers, body)) = read_simple_http_request(&mut socket).await?
    else {
        return Ok(());
    };
    if !matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS")
        && !check_csrf_origin(headers.get("origin").map(String::as_str), &access).await
    {
        return write_error_response_with_status(
            &mut socket,
            "403 Forbidden",
            "CSRF_REJECTED",
            "cross-origin request blocked",
        )
        .await;
    }
    if !check_host_header(headers.get("host").map(String::as_str), &access).await {
        return write_error_response_with_status(
            &mut socket,
            "403 Forbidden",
            "WEBUI_HOST_REJECTED",
            "host header not allowed",
        )
        .await;
    }
    match (method.as_str(), target.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            write_html_page_response(&mut socket, &web_ui_html()).await
        }
        ("GET", "/favicon.svg") | ("GET", "/logo.svg") => {
            write_http_response(&mut socket, "200 OK", "image/svg+xml", web_ui_favicon_svg()).await
        }
        ("GET", "/favicon.ico") => {
            write_http_response(&mut socket, "200 OK", "image/x-icon", web_ui_favicon_ico()).await
        }
        ("GET", "/api/status") => {
            write_http_response(&mut socket, "200 OK", "application/json", &serde_json::to_vec(
                &json!({"success":true,"data":{
                    "storage_paused":true,"database_available":false,
                    "worker_loop_running":false,"worker_status":"paused_storage",
                    "restart_required":true,
                    "last_error":"STORAGE_CAPACITY_EXHAUSTED",
                    "last_event":"webui.capacity_paused"
                }}))?).await
        }
        ("GET", "/health" | "/api/health") => {
            write_http_response(&mut socket, "200 OK", "application/json",
                &serde_json::to_vec(&json!({"success":true,"data":{
                    "storage_paused":true,"uptime_seconds":start.elapsed().as_secs()
                }}))?).await
        }
        ("GET", "/api/storage/capacity") => storage::get(&mut socket).await,
        ("POST", "/api/storage/capacity") => storage::update(&mut socket, &body).await,
        _ => write_error_response_with_status(&mut socket, "503 Service Unavailable",
            "STORAGE_CAPACITY_EXHAUSTED",
            "Database access is paused by storage capacity. Existing evidence is retained. Increase capacity and restart; no work was submitted.").await,
    }
}

/// Return the site status shape exposed to the local browser UI.
///
/// `WebUiState` intentionally keeps secrets in memory for direct-WP requests,
/// but HTTP status/auth responses are not allowed to serialize them.  The
/// client can submit a replacement secret; it never needs the old value.
pub(super) fn redacted_domain_status_items(
    domains: &[DomainStatusItem],
    domain_token_bindings: &DomainTokenBindingsDoc,
) -> Vec<Value> {
    domains
        .iter()
        .map(|domain| {
            // §67 recheck (tasks/cursor CURSOR-COMMERCIAL-USE-RECHECK-20260929):
            // the status page offered no way to tell which plugin lane a
            // domain serves (wpmmcc-ats vs wpmmcc) — the identity lived only
            // inside the encrypted binding doc, so an operator could not
            // confirm a client was mounted on both lanes. Surface the lane
            // identity and its verification timestamp per domain, resolved
            // through the same key order the token/secret resolvers use.
            // Secret material (token / route secret) stays redacted.
            let binding =
                crate::bindings::resolve_entry_for_domain(&domain.api_base_url, domain_token_bindings);
            json!({
                "api_base_url": domain.api_base_url,
                "site_status": domain.site_status,
                "route_secret_set": domain.route_secret.as_deref().is_some_and(|v| !v.trim().is_empty()),
                "max_relations": domain.max_relations,
                "plan_expires_at": domain.plan_expires_at,
                "plugin_identity": binding
                    .and_then(|entry| entry.plugin_identity)
                    .map(|identity| identity.as_wire_str().to_string()),
                "identity_verified_at": binding
                    .and_then(|entry| entry.identity_verified_at.clone()),
            })
        })
        .collect()
}

fn sensitive_json_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    normalized.contains("secret")
        || normalized.contains("token")
        || normalized.contains("password")
        || normalized.contains("apikey")
        || normalized.contains("authorization")
        || normalized.contains("credential")
        || normalized.contains("privatekey")
        || normalized.contains("clientsecret")
        || normalized.contains("accesstoken")
        || normalized.contains("refreshtoken")
}

fn redact_sensitive_json(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields.iter_mut() {
                if sensitive_json_key(key) {
                    *child = Value::String("[redacted]".to_string());
                } else {
                    redact_sensitive_json(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_sensitive_json(item);
            }
        }
        _ => {}
    }
}

fn redacted_component_bindings(bindings: &ComponentBindingsDoc) -> Value {
    let mut value = serde_json::to_value(bindings).unwrap_or_else(|_| {
        json!({
            "version": bindings.version,
            "components": {},
        })
    });
    // `auth` is a free-form map, so its field names are useful to the UI but
    // none of its values may leave the process.
    if let Some(components) = value.get_mut("components").and_then(Value::as_object_mut) {
        for entry in components.values_mut() {
            if let Some(auth) = entry.get_mut("auth").and_then(Value::as_object_mut) {
                for child in auth.values_mut() {
                    *child = Value::String("[configured]".to_string());
                }
            }
        }
    }
    redact_sensitive_json(&mut value);
    value
}

async fn handle_get_status(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    // Local mode must not serialize a server URL or session metadata. The
    // legacy projection is nested and explicitly named, and still redacts
    // the session token to a masked prefix.
    let legacy_control_plane = crate::config::server_control_plane_enabled();
    let payload = {
        let guard = state.lock().await;
        let mut data = json!({
            "runtime_mode": crate::config::runtime_mode(),
            "device_id": guard.device_id.clone(),
            "domains": redacted_domain_status_items(&guard.domains, &guard.domain_token_bindings),
            "component_bindings_path": guard.component_bindings_path.clone(),
            "component_bindings": redacted_component_bindings(&guard.component_bindings),
            "domain_token_bindings_path": guard.domain_token_bindings_path.clone(),
            "domain_token_bindings": domain_token_binding_status_items(&guard.domain_token_bindings),
            "task_type_component_bindings_path": guard.task_type_component_bindings_path.clone(),
            "task_type_component_bindings": task_type_component_binding_status_items(&guard.task_type_component_bindings),
            "rule_component_bindings_path": guard.rule_component_bindings_path.clone(),
            "rule_component_bindings": rule_component_binding_status_items(&guard.rule_component_bindings),
            "worker_loop_running": guard.worker_loop_running,
            "worker_status": guard.worker_status.clone(),
            "worker_loop_poll_seconds": guard.worker_loop_poll_seconds,
            "worker_last_summary": guard.worker_last_summary.clone(),
            "worker_recent_runs": guard.worker_recent_runs.clone(),
            "local_components_backfilled": guard.local_components_backfilled,
            "local_components_backfill_error": guard.local_components_backfill_error.clone(),
            "last_error": guard.last_error.clone(),
            "last_event": guard.last_event.clone(),
            "updated_at": guard.updated_at,
            // S12 (batch G): surfaced so the UI can render a prominent
            // warning banner while insecure TLS is enabled (default false).
            "allow_insecure_tls": crate::config::env_bool(
                "WPTSALL_ALLOW_INSECURE_TLS",
                false,
            ),
        });
        if legacy_control_plane {
            data["server_base"] = json!(guard.server_base.clone());
            data["logged_in"] = json!(guard.session_token.is_some());
            data["session_token_prefix"] = json!(guard
                .session_token
                .as_deref()
                .map(session_token_prefix)
                .unwrap_or_default());
            data["components"] = json!(guard.components.clone());
            data["legacy_control_plane"] = json!({
                "configured": true,
                "logged_in": guard.session_token.is_some(),
                "session_token_prefix": guard
                    .session_token
                    .as_deref()
                    .map(session_token_prefix)
                    .unwrap_or_default(),
            });
        }
        json!({ "success": true, "data": data })
    };
    let encoded = serde_json::to_vec(&payload)?;
    write_http_response(socket, "200 OK", "application/json", &encoded).await
}

async fn handle_health(
    socket: &mut TcpStream,
    start_time: std::time::Instant,
) -> anyhow::Result<()> {
    let uptime_seconds = start_time.elapsed().as_secs();
    // Envelope contract (批 O3 疤): /health used to return a FLAT payload —
    // the only web UI endpoint outside the {"success": true, "data": …}
    // convention. The frontend's isOk() therefore rejected it and the About
    // tab's pre-check version block never rendered (masked by an API-level
    // mock in Settings.test.ts that shaped the response as if wrapped).
    // Wrap it like every other endpoint; the fields move under "data".
    let payload = json!({
        "success": true,
        "data": {
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            // On-disk UI bundle version (VERSION-WEBUI, else binary version).
            // Local-only read: lets the About tab pre-display versions without
            // contacting the update server.
            "ui_version": crate::updater::current_ui_version(),
            "uptime_seconds": uptime_seconds
        }
    });
    let encoded = serde_json::to_vec(&payload)?;
    write_http_response(socket, "200 OK", "application/json", &encoded).await
}

async fn handle_translation_retry(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id_str: &str,
) -> anyhow::Result<()> {
    let id: i64 = match id_str.parse() {
        Ok(v) if v > 0 => v,
        _ => {
            return write_error_response(
                socket,
                "INVALID_ID",
                "translation id must be a positive integer",
            )
            .await;
        }
    };

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };

    // Look up the translation record and verify it's failed.
    let result = {
        let conn = db_arc.lock().await;
        (|| -> anyhow::Result<Option<bool>> {
            let Some(record) = crate::db::translations::get_translation_record_by_id(&conn, id)?
            else {
                return Ok(None);
            };
            if record.status != "failed" {
                return Ok(None); // Only failed records can be retried
            }
            if record.relation_id.is_none() || record.object_id.is_none() {
                return Ok(None); // Need relation_id and object_id for retry
            }
            Ok(Some(crate::db::translations::insert_retry_queue_entry(
                &conn, &record,
            )?))
        })()
    };

    match result {
        Ok(Some(queued)) => {
            let payload = json!({ "success": true, "data": { "queued": queued } });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Ok(None) => {
            write_error_response(
                socket,
                "RETRY_FAILED",
                "Record not found, not in failed status, or missing relation/object ID",
            )
            .await
        }
        Err(error) => {
            write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "RETRY_ENQUEUE_FAILED",
                &err_public(&error),
            )
            .await
        }
    }
}

// --- helpers ---

fn cached_route_secret_for_domain(
    domains: &[DomainStatusItem],
    domain_key: &str,
) -> Option<String> {
    domains
        .iter()
        .find(|domain| normalize_domain_base(&domain.api_base_url) == domain_key)
        .and_then(|domain| domain.route_secret.clone())
        .filter(|secret| !secret.trim().is_empty())
}

async fn resolve_effective_route_secret_for_domain(
    state: &Arc<Mutex<WebUiState>>,
    domain_token_bindings: &DomainTokenBindingsDoc,
    domain: &str,
) -> anyhow::Result<Option<String>> {
    let domain_key = normalize_domain_base(domain);
    if domain_key.is_empty() {
        return Ok(None);
    }

    // P0-LF-03 5.4: local mode resolves route secrets ONLY from the local
    // domain-token/site-connection state. Neither the cached server domain
    // snapshot nor the website is consulted; a missing secret is an explicit
    // None the caller surfaces as a local MISSING_ROUTE_SECRET error.
    if !crate::config::server_control_plane_enabled() {
        return Ok(crate::bindings::resolve_route_secret_for_domain(
            &domain_key,
            domain_token_bindings,
        ));
    }

    let (cached_route_secret, server_base, session_token, client) = {
        let guard = state.lock().await;
        (
            cached_route_secret_for_domain(&guard.domains, &domain_key),
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    if cached_route_secret.is_some() {
        return Ok(cached_route_secret);
    }

    if let Some(route_secret) =
        crate::bindings::resolve_route_secret_for_domain(&domain_key, domain_token_bindings)
    {
        return Ok(Some(route_secret));
    }

    let Some(session_token) = session_token else {
        return Ok(None);
    };

    let refreshed_domains =
        fetch_domains_for_session(&client, &server_base, &session_token).await?;
    let refreshed_route_secret = cached_route_secret_for_domain(&refreshed_domains, &domain_key);
    {
        let mut guard = state.lock().await;
        guard.domains = refreshed_domains;
    }
    Ok(refreshed_route_secret)
}

async fn update_state_error(state: &Arc<Mutex<WebUiState>>, err: &anyhow::Error, event: &str) {
    let mut guard = state.lock().await;
    guard.last_error = format!("{:#}", err);
    guard.last_event = event.to_string();
    guard.updated_at = unix_ts();
}

// --- Local component instance CRUD + test ---

async fn handle_dynamic_routes(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    method: &str,
    target: &str,
    query: &str,
    body: &[u8],
) -> anyhow::Result<bool> {
    // PUT/DELETE /api/components/local/:id
    if let Some(rest) = target.strip_prefix("/api/components/local/") {
        // GET /api/components/local/:id/export
        if let Some(comp_id) = rest.strip_suffix("/export") {
            if method == "GET" && !comp_id.is_empty() && !comp_id.contains('/') {
                handle_local_component_export(socket, state, comp_id).await?;
                return Ok(true);
            }
        }
        if let Some(comp_id) = rest.strip_suffix("/refresh-snapshot") {
            if method == "POST" && !comp_id.is_empty() && !comp_id.contains('/') {
                handle_local_component_refresh_snapshot(socket, state, comp_id).await?;
                return Ok(true);
            }
        }
        // POST /api/components/local/:id/quick-test
        if let Some(comp_id) = rest.strip_suffix("/quick-test") {
            if method == "POST" && !comp_id.is_empty() && !comp_id.contains('/') {
                handle_local_component_quick_test(socket, state, body, comp_id).await?;
                return Ok(true);
            }
        }
        // POST /api/components/local/:id/versions
        // POST /api/components/local/:id/versions/:ver/test
        // PUT/DELETE /api/components/local/:id/versions/:ver
        if let Some((comp_id, versions_path)) = rest.split_once("/versions") {
            if versions_path.is_empty() || versions_path == "/" {
                // POST /api/components/local/:id/versions
                if method == "POST" {
                    handle_component_version_create(socket, state, body, comp_id).await?;
                    return Ok(true);
                }
            } else if let Some(ver_rest) = versions_path.strip_prefix('/') {
                if ver_rest.ends_with("/test") {
                    // POST /api/components/local/:id/versions/:ver/test
                    let ver = ver_rest.strip_suffix("/test").unwrap_or("");
                    if method == "POST" && !ver.is_empty() {
                        handle_component_version_test(socket, state, body, comp_id, ver).await?;
                        return Ok(true);
                    }
                } else if !ver_rest.is_empty() {
                    // PUT/DELETE /api/components/local/:id/versions/:ver
                    match method {
                        "PUT" => {
                            handle_component_version_update(socket, state, body, comp_id, ver_rest)
                                .await?;
                            return Ok(true);
                        }
                        "DELETE" => {
                            handle_component_version_delete(socket, state, comp_id, ver_rest)
                                .await?;
                            return Ok(true);
                        }
                        _ => {}
                    }
                }
            }
        } else if !rest.is_empty() && !rest.contains('/') {
            // PUT/DELETE /api/components/local/:id
            match method {
                "GET" => {
                    handle_local_component_detail(socket, state, rest).await?;
                    return Ok(true);
                }
                "PUT" => {
                    handle_local_component_update_v2(socket, state, body, rest).await?;
                    return Ok(true);
                }
                "DELETE" => {
                    handle_local_component_delete_v2(socket, state, rest).await?;
                    return Ok(true);
                }
                _ => {}
            }
        }
    }

    // PUT/DELETE /api/vendor-keys/:id
    if let Some(rest) = target.strip_prefix("/api/vendor-keys/") {
        if !rest.is_empty() && !rest.contains('/') {
            match method {
                "PUT" => {
                    handle_vendor_key_update(socket, state, body, rest).await?;
                    return Ok(true);
                }
                "DELETE" => {
                    handle_vendor_key_delete(socket, state, rest).await?;
                    return Ok(true);
                }
                _ => {}
            }
        }
    }

    // PUT/DELETE /api/vendor-oauth/:id  |  POST /api/vendor-oauth/:id/authorize
    if let Some(rest) = target.strip_prefix("/api/vendor-oauth/") {
        if rest.ends_with("/authorize") {
            let oauth_id = rest.strip_suffix("/authorize").unwrap_or("");
            if method == "POST" && !oauth_id.is_empty() {
                handle_oauth_authorize(socket, state, oauth_id).await?;
                return Ok(true);
            }
        } else if !rest.is_empty() && !rest.contains('/') {
            match method {
                "PUT" => {
                    handle_oauth_config_update(socket, state, body, rest).await?;
                    return Ok(true);
                }
                "DELETE" => {
                    handle_oauth_config_delete(socket, state, rest).await?;
                    return Ok(true);
                }
                _ => {}
            }
        }
    }

    // PUT/DELETE /api/proxy-profiles/:id  |  POST /api/proxy-profiles/:id/test
    if let Some(rest) = target.strip_prefix("/api/proxy-profiles/") {
        if rest.ends_with("/test") {
            let proxy_id = rest.strip_suffix("/test").unwrap_or("");
            if method == "POST" && !proxy_id.is_empty() {
                handle_proxy_test(socket, state, proxy_id).await?;
                return Ok(true);
            }
        } else if !rest.is_empty() && !rest.contains('/') {
            match method {
                "PUT" => {
                    handle_proxy_update(socket, state, body, rest).await?;
                    return Ok(true);
                }
                "DELETE" => {
                    handle_proxy_delete(socket, state, rest).await?;
                    return Ok(true);
                }
                _ => {}
            }
        }
    }

    // POST /api/translations/:id/retry
    if let Some(rest) = target.strip_prefix("/api/translations/") {
        if rest.ends_with("/retry") {
            let id_str = rest.strip_suffix("/retry").unwrap_or("");
            if method == "POST" && !id_str.is_empty() {
                handle_translation_retry(socket, state, id_str).await?;
                return Ok(true);
            }
        }
    }

    // PUT /api/discovery-tasks/:id
    if let Some(rest) = target.strip_prefix("/api/discovery-tasks/") {
        if !rest.is_empty() && !rest.contains('/') && method == "PUT" {
            handle_discovery_task_update(socket, state, body, rest).await?;
            return Ok(true);
        }
    }

    // GET /api/items/pending-review — aggregated inbox (before /api/items/:id)
    if method == "GET" && target == "/api/items/pending-review" {
        handle_items_pending_review(socket, state, query).await?;
        return Ok(true);
    }

    // POST /api/items/batch-approve | batch-reject (must be before /api/items/:id)
    if method == "POST" && target == "/api/items/batch-approve" {
        handle_items_batch_approve(socket, state, body).await?;
        return Ok(true);
    }
    if method == "POST" && target == "/api/items/batch-reject" {
        handle_items_batch_reject(socket, state, body).await?;
        return Ok(true);
    }

    // GET /api/items/:id/content  |  PUT /api/items/:id/translated
    // PUT /api/items/:id/override | POST /api/items/:id/retranslate
    // POST /api/items/:id/resubmit | POST /api/items/:id/approve
    if let Some(rest) = target.strip_prefix("/api/items/") {
        if rest.ends_with("/content") {
            let id_str = rest.strip_suffix("/content").unwrap_or("");
            if method == "GET" && !id_str.is_empty() {
                handle_item_content(socket, state, id_str).await?;
                return Ok(true);
            }
        } else if rest.ends_with("/override") {
            let id_str = rest.strip_suffix("/override").unwrap_or("");
            if method == "PUT" && !id_str.is_empty() {
                handle_item_override_save(socket, state, body, id_str).await?;
                return Ok(true);
            }
        } else if rest.ends_with("/retranslate") {
            let id_str = rest.strip_suffix("/retranslate").unwrap_or("");
            if method == "POST" && !id_str.is_empty() {
                handle_item_retranslate(socket, state, id_str, body).await?;
                return Ok(true);
            }
        } else if rest.ends_with("/translated") {
            let id_str = rest.strip_suffix("/translated").unwrap_or("");
            if method == "PUT" && !id_str.is_empty() {
                handle_item_translated_save(socket, state, body, id_str).await?;
                return Ok(true);
            }
        } else if rest.ends_with("/resubmit") {
            let id_str = rest.strip_suffix("/resubmit").unwrap_or("");
            if method == "POST" && !id_str.is_empty() {
                handle_item_resubmit(socket, state, id_str).await?;
                return Ok(true);
            }
        } else if rest.ends_with("/approve") {
            let id_str = rest.strip_suffix("/approve").unwrap_or("");
            if method == "POST" && !id_str.is_empty() {
                handle_item_approve(socket, state, id_str).await?;
                return Ok(true);
            }
        } else if rest.ends_with("/reject") {
            let id_str = rest.strip_suffix("/reject").unwrap_or("");
            if method == "POST" && !id_str.is_empty() {
                handle_item_reject(socket, state, id_str, body).await?;
                return Ok(true);
            }
        }
    }

    // GET /api/jobs/:id and GET /api/jobs/:id/items
    // `Tasks.svelte` expands a job by reading both the summary route and items route.
    if let Some(rest) = target.strip_prefix("/api/jobs/") {
        if rest.ends_with("/items") {
            let id_str = rest.strip_suffix("/items").unwrap_or("");
            if method == "GET" && !id_str.is_empty() {
                handle_job_items(socket, state, id_str, query).await?;
                return Ok(true);
            }
        } else if !rest.is_empty() && !rest.contains('/') && method == "GET" {
            handle_job_detail(socket, state, rest).await?;
            return Ok(true);
        }
    }

    // GET /api/sync-review | GET/PUT /api/sync-review/:id | POST .../approve|reject
    if method == "GET" && target == "/api/sync-review" {
        handle_sync_review_list(socket, state, query).await?;
        return Ok(true);
    }
    if let Some(rest) = target.strip_prefix("/api/sync-review/") {
        if rest.ends_with("/approve") {
            let id = rest.strip_suffix("/approve").unwrap_or("");
            if method == "POST" && !id.is_empty() && !id.contains('/') {
                handle_sync_review_approve(socket, state, id).await?;
                return Ok(true);
            }
        } else if rest.ends_with("/reject") {
            let id = rest.strip_suffix("/reject").unwrap_or("");
            if method == "POST" && !id.is_empty() && !id.contains('/') {
                handle_sync_review_reject(socket, state, id, body).await?;
                return Ok(true);
            }
        } else if !rest.is_empty() && !rest.contains('/') {
            if method == "GET" {
                handle_sync_review_detail(socket, state, rest).await?;
                return Ok(true);
            }
            if method == "PUT" {
                handle_sync_review_update(socket, state, rest, body).await?;
                return Ok(true);
            }
        }
    }

    // /api/sync-pairs/pair | /api/sync-pairs/credentials[/:domain]
    if target == "/api/sync-pairs/pair" && method == "POST" {
        handle_sync_pairs_pair(socket, state, &body).await?;
        return Ok(true);
    }
    if target == "/api/sync-pairs/credentials" && method == "GET" {
        handle_sync_pair_credentials_list(socket, state).await?;
        return Ok(true);
    }
    if let Some(domain) = target.strip_prefix("/api/sync-pairs/credentials/") {
        if method == "DELETE" && !domain.is_empty() {
            // The frontend sends encodeURIComponent(domain); the raw wire
            // path is percent-encoded, so decode before matching.
            let domain = self::http::urldecode(domain);
            handle_sync_pair_credential_delete(socket, state, &domain).await?;
            return Ok(true);
        }
    }

    // /api/sync-pairs/:id/*
    if let Some(rest) = target.strip_prefix("/api/sync-pairs/") {
        if let Some(pair_id) = rest.strip_suffix("/run") {
            if method == "POST" && !pair_id.is_empty() && !pair_id.contains('/') {
                handle_sync_pairs_run(socket, state, pair_id).await?;
                return Ok(true);
            }
        } else if let Some(pair_id) = rest.strip_suffix("/pause") {
            if method == "POST" && !pair_id.is_empty() && !pair_id.contains('/') {
                handle_sync_pairs_pause(socket, state, pair_id).await?;
                return Ok(true);
            }
        } else if let Some(pair_id) = rest.strip_suffix("/resume") {
            if method == "POST" && !pair_id.is_empty() && !pair_id.contains('/') {
                handle_sync_pairs_resume(socket, state, pair_id).await?;
                return Ok(true);
            }
        } else if (method == "DELETE" || method == "POST")
            && !rest.is_empty()
            && !rest.contains('/')
        {
            handle_sync_pairs_delete(socket, state, rest).await?;
            return Ok(true);
        }
    }

    Ok(false)
}

// ---------------------------------------------------------------------------
// Discovery tasks
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Config file path helpers
// ---------------------------------------------------------------------------

fn vendor_keys_path() -> String {
    crate::config::vendor_keys_file()
}

fn vendor_oauth_path() -> String {
    crate::config::vendor_oauth_file()
}

fn proxy_profiles_path() -> String {
    crate::config::proxy_profiles_file()
}

fn components_local_path() -> String {
    crate::config::components_local_file()
}

fn web_ui_sqlite_storage_enabled() -> bool {
    std::env::var("WPTSALL_WEB_UI")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false)
}

async fn handle_vendors_list(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let (server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return write_session_required(socket).await;
    };
    match fetch_vendors_for_session(&client, &server_base, &session_token).await {
        Ok(vendors) => {
            let payload = json!({ "success": true, "data": { "items": vendors } });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            update_state_error(state, &err, "vendors.list_failed").await;
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            write_error_response(socket, "VENDORS_LIST_FAILED", &err_public(&err)).await
        }
    }
}

async fn handle_wp_translation_providers_list(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let (server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return write_session_required(socket).await;
    };
    let signing_key_pem = read_trusted_signing_key_pem_from_state(state).await;
    let items = match fetch_wp_translation_providers_for_session_with_signing_key(
        &client,
        &server_base,
        &session_token,
        signing_key_pem.as_deref(),
    )
    .await
    {
        Ok(items) => items,
        Err(first_err) if is_signature_related_error(&first_err) => {
            let refreshed = fetch_signing_key_from_server(&client, &server_base, &session_token)
                .await
                .with_context(|| {
                    "client wp-translation-providers: refresh signing key after verify failed"
                })?;
            persist_signing_key_material(state, &refreshed).await?;
            fetch_wp_translation_providers_for_session_with_signing_key(
                &client,
                &server_base,
                &session_token,
                Some(&refreshed.pem),
            )
            .await
            .with_context(|| {
                "client wp-translation-providers: verify/decrypt failed after key refresh"
            })?
        }
        Err(err) => {
            update_state_error(state, &err, "wp_translation_providers.list_failed").await;
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            return write_error_response(
                socket,
                "WP_TRANSLATION_PROVIDERS_LIST_FAILED",
                &err_public(&err),
            )
            .await;
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

async fn handle_cloud_api_types_list(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let (server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return write_session_required(socket).await;
    };
    let signing_key_pem = read_trusted_signing_key_pem_from_state(state).await;
    match fetch_cloud_api_types_for_session_with_signing_key(
        &client,
        &server_base,
        &session_token,
        signing_key_pem.as_deref(),
    )
    .await
    {
        Ok(items) => {
            let payload = json!({ "success": true, "data": { "items": items } });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            update_state_error(state, &err, "cloud_api_types.list_failed").await;
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            write_error_response(socket, "CLOUD_API_TYPES_LIST_FAILED", &err_public(&err)).await
        }
    }
}

// ---------------------------------------------------------------------------
// Local Components CRUD (v2 — components.json)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Stats overview
// ---------------------------------------------------------------------------

/// Check CSRF Origin header validity.
/// Returns true if the request should be allowed, false if it should be rejected.
///
/// Rules (S1/Y-2 fix, tasks/5.3falsh2/07-S1 + 12 批 A1):
/// - No Origin header → allowed in LOCAL mode only (loopback-bound server;
///   local tooling and test lanes speak plain HTTP without browser headers).
///   REJECTED in external mode — a non-browser client stripping Origin must
///   not bypass the CSRF face; external-mode API tooling must send an Origin
///   header on writes.
/// - Loopback Origin hosts (127.0.0.1 / localhost / ::1 — EXACT host match,
///   any port, either scheme) → always allowed. Prefix matching is gone:
///   `http://127.0.0.1.evil.com` and `http://localhost.evil.com` used to
///   pass `starts_with`.
/// - External mode + Origin host (IP) in whitelist → allowed.
/// - Everything else (malformed, non-IP hostnames, bad ports) → rejected.
pub(crate) async fn check_csrf_origin(
    origin: Option<&str>,
    access_control: &super::AccessControl,
) -> bool {
    let external = access_control.is_external().await;
    let Some(origin) = origin else {
        return !external; // No Origin header: local same-origin/tooling only
    };
    let lowered = origin.to_lowercase();
    let Some(host) = origin_host(&lowered) else {
        return false; // Malformed Origin → fail closed
    };
    if is_loopback_host(host) {
        return true;
    }
    if !external {
        return false;
    }
    // In external mode, allow origins from whitelisted IPs
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => access_control.is_allowed(ip).await,
        Err(_) => false,
    }
}

/// Exact loopback Origin hosts, always allowed (any port, either scheme).
fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// Extract and validate the host part of an Origin header value (input
/// already lowercased by the caller). Returns None for malformed values —
/// missing scheme, empty host, non-numeric port, unbalanced IPv6 brackets —
/// so callers fail closed.
fn origin_host(origin: &str) -> Option<&str> {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))?;
    let authority = rest.split(&['/', '?', '#'][..]).next()?;
    if authority.contains(']') {
        // IPv6 literal: [::1] or [::1]:8977
        let inner = authority.strip_prefix('[')?;
        let close = inner.find(']')?;
        let host = &inner[..close];
        let tail = &inner[close + 1..];
        if let Some(port) = tail.strip_prefix(':') {
            valid_port(port)?;
        } else if !tail.is_empty() {
            return None;
        }
        return Some(host);
    }
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    if let Some(p) = port {
        valid_port(p)?;
    }
    Some(host)
}

fn valid_port(port: &str) -> Option<()> {
    if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
        Some(())
    } else {
        None
    }
}

/// S4 (07 号 audit, 12 批 A2): Host-header allowlist — the DNS-rebinding
/// gate. Local mode allows loopback host names only. External mode also
/// allows the configured bind host and any `WPTSALL_WEB_UI_HOST_ALLOWLIST`
/// entries (comma-separated, for reverse-proxy deployments). Exact host
/// equality, port-insensitive: `127.0.0.1.evil.com` is not `127.0.0.1`.
pub(crate) async fn check_host_header(
    host_header: Option<&str>,
    access_control: &super::AccessControl,
) -> bool {
    let Some(value) = host_header else {
        return false;
    };
    let Some(host) = host_header_host(value) else {
        return false;
    };
    let external = access_control.is_external().await;
    let bind_host = crate::web_ui::web_ui_effective_bind_addr();
    let bind_host = host_header_host(&bind_host).map(|s| s.to_string());
    let allowlist = std::env::var("WPTSALL_WEB_UI_HOST_ALLOWLIST")
        .map(|raw| {
            raw.split(',')
                .map(|entry| entry.trim().to_string())
                .filter(|entry| !entry.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    host_header_allowed(host, external, bind_host.as_deref(), &allowlist)
}

/// Pure decision core of the Host allowlist (unit-testable without a live
/// AccessControl or process env). `host` must already be the extracted,
/// lowercased host part (see `host_header_host`).
fn host_header_allowed(
    host: &str,
    external: bool,
    bind_host: Option<&str>,
    allowlist: &[String],
) -> bool {
    if is_loopback_host(host) {
        return true;
    }
    if !external {
        return false;
    }
    if let Some(bind_host) = bind_host {
        if host == bind_host.trim().to_ascii_lowercase() {
            return true;
        }
    }
    allowlist
        .iter()
        .any(|entry| entry.eq_ignore_ascii_case(host))
}

/// Extract and validate the host part of a Host header (or bind address).
/// Handles bare hosts, `host:port`, and bracketed IPv6 `[::1]:port`.
/// Returns None for malformed values (empty host, non-numeric port,
/// unbalanced brackets, multi-colon unbracketed forms) so callers fail
/// closed.
fn host_header_host(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(rest) = value.strip_prefix('[') {
        // IPv6 literal: [::1] or [::1]:8977
        let (host, tail) = rest.split_once(']')?;
        if host.is_empty() {
            return None;
        }
        if let Some(port) = tail.strip_prefix(':') {
            valid_port(port)?;
        } else if !tail.is_empty() {
            return None;
        }
        return Some(host);
    }
    match value.split(':').count() {
        1 => Some(value),
        2 => {
            let (host, port) = value.rsplit_once(':')?;
            if host.is_empty() {
                return None;
            }
            valid_port(port)?;
            Some(host)
        }
        // Unbracketed multi-colon form (bare IPv6 is invalid in Host)
        _ => None,
    }
}

/// S2 (07 号 audit, 12 批 A2): token-gate decision. Pure so the decision
/// matrix is unit-testable without a live socket. Remote peers are always
/// gated in external mode; loopback peers keep only read access to the
/// access-control state (token recovery lane); local mode never gates;
/// static assets (non-/api targets) never gate.
fn webui_token_required(
    external: bool,
    peer: Option<std::net::IpAddr>,
    method: &str,
    target: &str,
) -> bool {
    if !external || !target.starts_with("/api") {
        return false;
    }
    if peer.map(|ip| ip.is_loopback()).unwrap_or(false) {
        return !(method.eq_ignore_ascii_case("GET") && target == "/api/access-control");
    }
    true
}

/// Constant-time equality for token comparison (no early exit on the first
/// differing byte; length mismatch still short-circuits, which leaks only
/// the token length — acceptable for a fixed-format 32-hex token).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests;

// ── update-check (lightweight, polls wptsall-server releases manifest) ─────

async fn handle_update_check(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let server_base = {
        let guard = state.lock().await;
        guard.server_base.clone()
    };
    let outcome = async {
        let http_client = crate::updater::default_http_client()?;
        crate::updater::check_for_update(&http_client, &server_base).await
    }
    .await;
    match &outcome {
        Ok(result) => {
            crate::logging::log_event_global(
                "info",
                "ota.update_checked",
                json!({
                    "update_available": result.update_available,
                    "latest_version": result.latest_version,
                }),
            );
        }
        Err(err) => {
            crate::logging::log_event_global(
                "warn",
                "ota.update_check_failed",
                json!({ "error": format!("{:#}", err) }),
            );
        }
    }
    let payload = match outcome {
        Ok(result) => json!({
            "success": true,
            "data": result,
        }),
        Err(e) => json!({
            "success": false,
            "error": {
                "code": "UPDATE_CHECK_FAILED",
                "message": e.to_string(),
            },
            // Local-only versions so the About tab can still show "you are
            // on vX" even when the manifest fetch fails.
            "data": {
                "current_version": env!("CARGO_PKG_VERSION"),
                "ui_current_version": crate::updater::current_ui_version(),
            }
        }),
    };
    let encoded = serde_json::to_vec(&payload)?;
    write_http_response(socket, "200 OK", "application/json", &encoded).await
}

// ── logs management (route-local, mirrors the update-check style) ─────────

/// POST /api/logs/clear — truncate the client log file. The "logs.cleared"
/// marker event is written AFTER the truncate so it becomes the first line
/// of the fresh file (audit trail of the clear action itself).
async fn handle_logs_clear(socket: &mut TcpStream, log_file: &str) -> anyhow::Result<()> {
    match crate::logging::clear_log_file(log_file) {
        Ok(()) => {
            crate::logging::log_event_global("info", "logs.cleared", json!({}));
            let payload = json!({ "success": true, "data": { "cleared": true } });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => write_error_response(socket, "LOG_CLEAR_FAILED", &err_public(&err)).await,
    }
}

/// POST /api/logs/client — client-side JS error ingestion (see dispatch arm).
/// Level is always error; the central redaction boundary sanitizes stack
/// text and message bodies.
async fn handle_logs_client(socket: &mut TcpStream, body: &[u8]) -> anyhow::Result<()> {
    let req: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(
                socket,
                "INVALID_JSON",
                "expected { \"message\": \"...\", ... } client error report",
            )
            .await;
        }
    };
    let message = req
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown client error")
        .to_string();
    let detail = json!({
        "kind": req.get("kind").and_then(serde_json::Value::as_str).unwrap_or("js_error"),
        "message": crate::logging::snippet(&message),
        "source": req.get("source").and_then(serde_json::Value::as_str),
        "lineno": req.get("lineno"),
        "colno": req.get("colno"),
        "stack": req
            .get("stack")
            .and_then(serde_json::Value::as_str)
            .map(crate::logging::snippet),
    });
    crate::logging::log_event_global("error", "webui.client_error", detail);
    let payload = json!({ "success": true, "data": { "recorded": true } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

// ── perform-update (binary self-replace OR UI-only disk swap) ─────────

async fn handle_perform_update(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let (server_base, update_flag) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.update_in_progress.clone(),
        )
    };
    let http_client = match crate::updater::default_http_client() {
        Ok(client) => client,
        Err(_) => {
            return write_error_response(socket, "UPDATE_CHECK_FAILED", "OTA transport unavailable")
                .await;
        }
    };

    // Concurrent update guard
    if update_flag.load(std::sync::atomic::Ordering::Relaxed) {
        return write_error_response(
            socket,
            "UPDATE_IN_PROGRESS",
            "an update is already in progress",
        )
        .await;
    }

    // Step 1: Check for update (binary + UI)
    let check = match crate::updater::check_for_update(&http_client, &server_base).await {
        Ok(r) => r,
        Err(e) => {
            return write_error_response(socket, "UPDATE_CHECK_FAILED", &err_public(&e)).await;
        }
    };

    if !check.update_available {
        return write_error_response(socket, "ALREADY_UP_TO_DATE", "no update available").await;
    }

    // Prefer binary when it is newer; otherwise apply UI-only bundle.
    let apply_ui_only = crate::updater::should_apply_ui_only(&check);

    // A self-replacing update is about to begin — audit it before any
    // destructive step so a failed swap is still traceable.
    crate::logging::log_event_global(
        "info",
        "ota.perform_update_started",
        json!({
            "latest_version": check.latest_version,
            "mode": if apply_ui_only { "ui_only" } else { "binary" },
        }),
    );

    update_flag.store(true, std::sync::atomic::Ordering::Relaxed);

    if apply_ui_only {
        return perform_ui_only_update(socket, &http_client, &check, &update_flag).await;
    }

    // Step 2: Resolve download URL (binary kit)
    let download_url = client_runtime_core::updater::resolve_url(
        &check.download_url_template,
        &check.latest_version,
    );
    let artifact_name = download_url
        .rsplit('/')
        .next()
        .unwrap_or("wptsall-client")
        .to_string();
    let sha256sums_url = {
        let template = check.signature_url_template.replace(".minisig", "");
        Some(client_runtime_core::updater::resolve_url(
            &template,
            &check.latest_version,
        ))
    };

    // Steps 3-5: Secure download + minisign + sha256
    let skip_security = client_runtime_core::env_helpers::env_bool("WPTSALL_SKIP_SECURITY", false);
    let tmp_binary = if let Some(gate) = crate::security::gate() {
        match wptsall_client_security::download_and_verify_update(
            gate,
            &http_client,
            &download_url,
            &check.latest_version,
            &check.current_version,
            &artifact_name,
            sha256sums_url.as_deref(),
        )
        .await
        {
            Ok(v) => v.binary_path,
            Err(e) => {
                update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                return write_error_response(
                    socket,
                    "SECURE_UPDATE_VERIFY_FAILED",
                    &err_public(&e),
                )
                .await;
            }
        }
    } else if !skip_security {
        update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
        return write_error_response(
            socket,
            "SECURITY_GATE_INACTIVE",
            "security gate inactive — refusing binary update (set WPTSALL_SKIP_SECURITY=1 for local/dev only)",
        )
        .await;
    } else {
        // Explicit SKIP_SECURITY=1 checksum-only path (local OTA smoke only)
        let signature_url = client_runtime_core::updater::resolve_url(
            &check.signature_url_template,
            &check.latest_version,
        );
        let sums_url = sha256sums_url.clone().unwrap_or(signature_url);
        let sha256_content = match http_client.get(&sums_url).send().await {
            Ok(resp) if resp.status().is_success() => resp.text().await.unwrap_or_default(),
            Ok(resp) => {
                update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                return write_error_response(
                    socket,
                    "SHA256SUMS_FETCH_FAILED",
                    &format!("HTTP {}", resp.status()),
                )
                .await;
            }
            Err(e) => {
                update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                return write_error_response(
                    socket,
                    "SHA256SUMS_FETCH_FAILED",
                    &err_public(&anyhow::Error::from(e)),
                )
                .await;
            }
        };
        let expected_hash =
            match client_runtime_core::updater::parse_sha256sums(&sha256_content, &artifact_name) {
                Some(h) => h,
                None => {
                    update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                    return write_error_response(
                        socket,
                        "CHECKSUM_NOT_FOUND",
                        &format!("no checksum entry for {}", artifact_name),
                    )
                    .await;
                }
            };
        let tmp = match client_runtime_core::updater::download_update(&http_client, &download_url)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                return write_error_response(socket, "DOWNLOAD_FAILED", &err_public(&e)).await;
            }
        };
        if let Err(e) = client_runtime_core::updater::verify_checksum(&tmp, &expected_hash).await {
            update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
            let _ = tokio::fs::remove_file(&tmp).await;
            return write_error_response(socket, "CHECKSUM_MISMATCH", &err_public(&e)).await;
        }
        match client_runtime_core::updater::extract_update_binary(&tmp) {
            Ok(bin) => {
                if bin != tmp {
                    let _ = std::fs::remove_file(&tmp);
                }
                bin
            }
            Err(e) => {
                update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                let _ = tokio::fs::remove_file(&tmp).await;
                return write_error_response(socket, "KIT_EXTRACT_FAILED", &err_public(&e)).await;
            }
        }
    };

    // Step 6: Self-replace
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
            let _ = tokio::fs::remove_file(&tmp_binary).await;
            return write_error_response(
                socket,
                "CURRENT_EXE_FAILED",
                &err_public(&anyhow::Error::from(e)),
            )
            .await;
        }
    };

    if let Err(e) = client_runtime_core::updater::perform_self_replace(
        client_runtime_core::updater::webui_service_name(),
        &current_exe,
        &tmp_binary,
        false,
    ) {
        update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = tokio::fs::remove_file(&tmp_binary).await;
        // The runtime-core pre-flight marks unwritable install directories
        // with a stable prefix so the wire can carry a distinct code (the
        // generic spawn failure stays SELF_REPLACE_FAILED).
        let code = if format!("{e:#}").starts_with("install dir not writable:") {
            "INSTALL_DIR_NOT_WRITABLE"
        } else {
            "SELF_REPLACE_FAILED"
        };
        return write_error_response(socket, code, &err_public(&e)).await;
    }

    // Success — service will restart shortly
    let payload = json!({
        "success": true,
        "data": {
            "message": "Update initiated, restarting",
            "current_version": check.current_version,
            "target_version": check.latest_version,
            "update_kind": "binary",
        }
    });
    let encoded = serde_json::to_vec(&payload)?;
    let response = write_http_response(socket, "200 OK", "application/json", &encoded).await;

    // Exit so the self-replace helper can finish the swap (BUG-UPD-01):
    // Windows restarts via the helper's PowerShell; Linux/macOS webui
    // installs restart through systemd/launchd, and the helper additionally
    // nohup-relaunches the binary with the inherited environment when no
    // service unit exists (manual runs, bare runners). Staying alive is not
    // an option on macOS anyway: Darwin refuses both rename onto and unlink
    // of a running executable (ETXTBSY), so the swap could never happen
    // while this process lives.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(400));
        std::process::exit(0);
    });

    response
}

/// Download UI-only bundle, verify checksum, replace `ui/webui/` under install root.
async fn perform_ui_only_update(
    socket: &mut TcpStream,
    http_client: &reqwest::Client,
    check: &crate::updater::UpdateCheckResult,
    update_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    let target = check.ui_latest_version.clone();
    match crate::updater::download_verify_apply_ui(
        http_client,
        check,
        "webui",
        crate::security::gate(),
    )
    .await
    {
        Ok(install_root) => {
            update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
            let payload = json!({
                "success": true,
                "data": {
                    "message": "UI update applied (no restart required)",
                    "update_kind": "ui",
                    "current_version": check.ui_current_version,
                    "target_version": target,
                    "ui_path": install_root.join("ui/webui").display().to_string(),
                }
            });
            let encoded = serde_json::to_vec(&payload)?;
            write_http_response(socket, "200 OK", "application/json", &encoded).await
        }
        Err(e) => {
            update_flag.store(false, std::sync::atomic::Ordering::Relaxed);
            write_error_response(socket, "UI_APPLY_FAILED", &err_public(&e)).await
        }
    }
}
