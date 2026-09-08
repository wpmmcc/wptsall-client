use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::auth::{is_auth_error_message, UpstreamApiError};
use crate::bindings::has_any_domain_token_bindings;
use crate::component_rt::loader::{
    collect_configured_runtime_component_ids, load_component_runtimes,
};
use crate::config::{env_u64, env_usize};
use crate::logging::{log_event, snippet, unix_ts};
use crate::types::{WebUiRuntimeControl, WebUiState, WebUiWorkerConfigRequest};
use crate::web_ui::{
    fetch_domains_for_session, local_sites_from_domain_token_bindings, web_ui_run_worker_once,
    web_ui_run_worker_once_with_limits,
};

use super::errors::{
    write_error_response, write_error_response_with_status, write_session_required,
};
use super::http::write_http_response;

const LOCAL_DEV_LICENSE_STATUS: &str = "local_dev";

#[derive(Debug, Deserialize, Default)]
struct WorkerStartRequest {
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Serialize, Clone, Default)]
struct WorkerStartPreflightSummary {
    domains_checked: usize,
    relations_checked: usize,
    rules_checked: usize,
    fields_checked: usize,
    language_pack_lanes_checked: usize,
    blocking_missing_components: usize,
    confirm_missing_components: usize,
    auto_skip_missing_components: usize,
}

#[derive(Debug, Serialize, Clone)]
struct WorkerStartMissingComponent {
    api_base_url: String,
    business_line: String,
    relation_id: i64,
    rule_id: Option<i64>,
    source_group: String,
    routing_profile: String,
    delivery_target: String,
    object_name: String,
    field_name: String,
    source_role: String,
    preflight_policy: String,
    missing_component_behavior: String,
    severity: String,
    content_format: String,
    required_slot_key: String,
    suggested_task_type: String,
    input_artifact_kind: Option<String>,
    expected_output_artifact_kind: Option<String>,
}

#[derive(Debug, Serialize, Clone, Default)]
struct WorkerStartPreflightData {
    can_start: bool,
    requires_confirmation: bool,
    summary: WorkerStartPreflightSummary,
    missing_components: Vec<WorkerStartMissingComponent>,
}

#[derive(Debug, Deserialize, Default)]
struct WorkerRunOnceRequest {
    max_iterations: Option<usize>,
    max_elapsed_secs: Option<u64>,
    max_items_per_run: Option<usize>,
}

fn is_run_once_pressure_error(message: &str, upstream: Option<&UpstreamApiError>) -> bool {
    if let Some(upstream) = upstream {
        let status = upstream.status.to_lowercase();
        let code = upstream.code.to_lowercase();
        if status.contains("429")
            || status.contains("502")
            || status.contains("503")
            || status.contains("504")
            || code == "rate_limited"
        {
            return true;
        }
    }

    let lower = message.to_lowercase();
    lower.contains("rate_limited")
        || lower.contains("too many requests")
        || lower.contains("status=429")
        || lower.contains("status 429")
        || lower.contains("status=502")
        || lower.contains("status 502")
        || lower.contains("status=503")
        || lower.contains("status 503")
        || lower.contains("status=504")
        || lower.contains("status 504")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection reset")
}

pub(super) async fn handle_worker_run_once(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    log_file: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WorkerRunOnceRequest = if body.is_empty() {
        WorkerRunOnceRequest::default()
    } else {
        serde_json::from_slice(body).with_context(|| "invalid /api/worker/run-once json payload")?
    };
    let started_at = std::time::Instant::now();
    let mut iterations: usize = 0;
    let max_iterations = req
        .max_iterations
        .unwrap_or_else(|| env_usize("WPTSALL_RUN_ONCE_MAX_ITERATIONS", 180))
        .max(1);
    let max_elapsed_secs = req
        .max_elapsed_secs
        .unwrap_or_else(|| env_u64("WPTSALL_RUN_ONCE_MAX_ELAPSED_SECS", 900))
        .max(30);
    let mut break_reason: Option<&'static str> = None;

    {
        let mut guard = state.lock().await;
        guard.worker_status = "running_manual".to_string();
        guard.worker_loop_running = true;
        guard.last_event = "worker.run_once.started".to_string();
        guard.updated_at = unix_ts();
    }

    let mut acc_processed: i64 = 0;
    let mut acc_succeeded: i64 = 0;
    let mut acc_failed: i64 = 0;
    let mut last_summary = json!({});
    let mut last_err: Option<String> = None;
    let mut last_upstream_error: Option<UpstreamApiError> = None;

    loop {
        iterations += 1;
        match web_ui_run_worker_once_with_limits(state, log_file, req.max_items_per_run).await {
            Ok(summary) => {
                let total = summary
                    .get("total_items")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let succeeded = summary
                    .get("tasks_succeeded")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let failed = summary
                    .get("tasks_failed")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let _ = log_event(
                    log_file,
                    "info",
                    "worker.run_once.iteration",
                    json!({
                        "iteration": iterations,
                        "total_items": total,
                        "tasks_succeeded": succeeded,
                        "tasks_failed": failed
                    }),
                );
                acc_processed += total;
                acc_succeeded += succeeded;
                acc_failed += failed;
                last_summary = summary;
                if total == 0 {
                    break_reason = Some("empty_queue");
                    let _ = log_event(
                        log_file,
                        "info",
                        "worker.run_once.break",
                        json!({ "reason": "empty_queue", "iteration": iterations }),
                    );
                    break;
                }
                if succeeded == 0 && failed == 0 {
                    break_reason = Some("dedup_or_noop");
                    let _ = log_event(
                        log_file,
                        "info",
                        "worker.run_once.break",
                        json!({ "reason": "dedup_or_noop", "iteration": iterations }),
                    );
                    break;
                }
                if iterations >= max_iterations {
                    break_reason = Some("max_iterations");
                    let _ = log_event(
                        log_file,
                        "warning",
                        "worker.run_once.break",
                        json!({
                            "reason": "max_iterations",
                            "iteration": iterations,
                            "max_iterations": max_iterations
                        }),
                    );
                    break;
                }
                if started_at.elapsed().as_secs() >= max_elapsed_secs {
                    break_reason = Some("max_elapsed");
                    let _ = log_event(
                        log_file,
                        "warning",
                        "worker.run_once.break",
                        json!({
                            "reason": "max_elapsed",
                            "iteration": iterations,
                            "max_elapsed_secs": max_elapsed_secs
                        }),
                    );
                    break;
                }
            }
            Err(err) => {
                let upstream_error = err.downcast_ref::<UpstreamApiError>().cloned();
                let err_text = format!("{:#}", err);
                let pressure_after_progress = acc_succeeded > 0
                    && acc_failed == 0
                    && is_run_once_pressure_error(&err_text, upstream_error.as_ref());
                let _ = log_event(
                    log_file,
                    if pressure_after_progress {
                        "warning"
                    } else {
                        "error"
                    },
                    "worker.run_once.error",
                    json!({
                        "iteration": iterations,
                        "pressure_after_progress": pressure_after_progress,
                        "tasks_processed": acc_processed,
                        "tasks_succeeded": acc_succeeded,
                        "tasks_failed": acc_failed,
                        "error": snippet(&err_text)
                    }),
                );
                if pressure_after_progress {
                    break_reason = Some("upstream_pressure_after_progress");
                    if let Some(obj) = last_summary.as_object_mut() {
                        obj.insert(
                            "break_reason".to_string(),
                            json!("upstream_pressure_after_progress"),
                        );
                        obj.insert("pressure_error".to_string(), json!(snippet(&err_text)));
                    }
                    break;
                }
                last_upstream_error = upstream_error;
                last_err = Some(err_text);
                break;
            }
        }
    }

    if acc_processed > 0 {
        if let Some(obj) = last_summary.as_object_mut() {
            obj.insert("tasks_processed".to_string(), json!(acc_processed));
            obj.insert("tasks_succeeded".to_string(), json!(acc_succeeded));
            obj.insert("tasks_failed".to_string(), json!(acc_failed));
            obj.insert("total_items".to_string(), json!(acc_processed));
            if let Some(reason) = break_reason {
                obj.insert("break_reason".to_string(), json!(reason));
            }
        }
    }

    {
        let mut guard = state.lock().await;
        guard.worker_loop_running = false;
        guard.updated_at = unix_ts();
        if last_err.is_none() {
            guard.worker_status = "completed".to_string();
            guard.last_event = "worker.run_once.completed".to_string();
        } else {
            guard.worker_status = "error".to_string();
            guard.last_event = "worker.run_once.failed".to_string();
        }
    }

    match last_err {
        None => {
            let _ = log_event(
                log_file,
                "info",
                "worker.run_once.completed",
                json!({
                    "iterations": iterations,
                    "elapsed_ms": started_at.elapsed().as_millis() as u64,
                    "break_reason": break_reason.unwrap_or("unknown"),
                    "tasks_processed": acc_processed,
                    "tasks_succeeded": acc_succeeded,
                    "tasks_failed": acc_failed
                }),
            );
            let payload = json!({ "success": true, "data": last_summary });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Some(message) => {
            let _ = log_event(
                log_file,
                "error",
                "worker.run_once.failed",
                json!({
                    "iterations": iterations,
                    "elapsed_ms": started_at.elapsed().as_millis() as u64,
                    "tasks_processed": acc_processed,
                    "tasks_succeeded": acc_succeeded,
                    "tasks_failed": acc_failed,
                    "error": snippet(&message)
                }),
            );
            if let Some(upstream) = last_upstream_error {
                return write_error_response_with_status(
                    socket,
                    &upstream.status,
                    &upstream.code,
                    &upstream.message,
                )
                .await;
            }
            let code = if message.contains("MISSING_WP_CLIENT_TOKEN") {
                "MISSING_WP_CLIENT_TOKEN"
            } else {
                "WORKER_RUN_FAILED"
            };
            write_error_response(socket, code, &message).await
        }
    }
}

pub(super) async fn handle_worker_start(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    runtime_control: &WebUiRuntimeControl,
    log_file: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let request = parse_worker_start_request(body);
    let use_server_control_plane = crate::config::server_control_plane_enabled();
    let has_server_session = {
        let guard = state.lock().await;
        guard
            .session_token
            .as_deref()
            .map(str::trim)
            .is_some_and(|token| !token.is_empty())
    };
    if use_server_control_plane && !has_server_session {
        return write_session_required(socket).await;
    }

    let wp_client_token = if use_server_control_plane {
        crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "")
    } else {
        String::new()
    };
    let has_domain_token_bindings = {
        let guard = state.lock().await;
        has_any_domain_token_bindings(&guard.domain_token_bindings)
    };
    if use_server_control_plane && wp_client_token.trim().is_empty() && !has_domain_token_bindings {
        return write_error_response(
            socket,
            "MISSING_WP_CLIENT_TOKEN",
            "Set WPTSALL_WP_CLIENT_TOKEN or configure domain token bindings before starting worker loop",
        )
        .await;
    }

    if !request.force {
        let preflight = match collect_worker_start_preflight_bounded(state, log_file).await? {
            PreflightOutcome::Ready(preflight) => preflight,
            PreflightOutcome::TimedOut { seconds } => {
                return write_preflight_timeout_response(socket, seconds).await;
            }
        };
        if !preflight.can_start {
            let payload = json!({
                "success": false,
                "error": {
                    "code": "WORKER_START_BLOCKED_BY_POLICY",
                    "message": format!(
                        "发现 {} 个缺失组件项被当前 relation 策略标记为必须阻断，补齐组件后才能继续启动",
                        preflight.summary.blocking_missing_components
                    )
                },
                "data": preflight,
            });
            return write_http_response(
                socket,
                "409 Conflict",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await;
        }
        if preflight.requires_confirmation {
            let payload = json!({
                "success": false,
                "error": {
                    "code": "WORKER_START_CONFIRM_REQUIRED",
                    "message": format!(
                        "发现 {} 个翻译字段/语言包 lane 缺少对应组件，确认后才能继续启动",
                        preflight.missing_components.len()
                    )
                },
                "data": preflight,
            });
            return write_http_response(
                socket,
                "409 Conflict",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await;
        }
    } else {
        let preflight = match collect_worker_start_preflight_bounded(state, log_file).await? {
            PreflightOutcome::Ready(preflight) => preflight,
            PreflightOutcome::TimedOut { seconds } => {
                return write_preflight_timeout_response(socket, seconds).await;
            }
        };
        if !preflight.can_start {
            let payload = json!({
                "success": false,
                "error": {
                    "code": "WORKER_START_BLOCKED_BY_POLICY",
                    "message": format!(
                        "发现 {} 个缺失组件项被当前 relation 策略标记为必须阻断，force 启动也不会绕过",
                        preflight.summary.blocking_missing_components
                    )
                },
                "data": preflight,
            });
            return write_http_response(
                socket,
                "409 Conflict",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await;
        }
    }

    let mut handle_guard = runtime_control.worker_handle.lock().await;
    if let Some(handle) = handle_guard.as_ref() {
        if handle.is_finished() {
            *handle_guard = None;
        }
    }
    if runtime_control.worker_running.load(Ordering::SeqCst) {
        let poll = {
            let guard = state.lock().await;
            guard.worker_loop_poll_seconds
        };
        let payload = json!({
            "success": true,
            "data": { "running": true, "already_running": true, "poll_seconds": poll }
        });
        write_http_response(
            socket,
            "200 OK",
            "application/json",
            &serde_json::to_vec(&payload)?,
        )
        .await?;
        return Ok(());
    }

    runtime_control.worker_running.store(true, Ordering::SeqCst);
    let poll_seconds = {
        let mut guard = state.lock().await;
        guard.worker_loop_running = true;
        guard.worker_status = "running_auto".to_string();
        guard.last_error.clear();
        guard.last_event = "worker.loop.started".to_string();
        guard.updated_at = unix_ts();
        guard.worker_loop_poll_seconds
    };

    let loop_state = Arc::clone(state);
    let loop_runtime = runtime_control.clone();
    let loop_log_file = log_file.to_string();
    let join = spawn_worker_loop_task(loop_state, loop_runtime, loop_log_file, poll_seconds).await;
    *handle_guard = Some(join);
    drop(handle_guard);

    let payload = json!({
        "success": true,
        "data": { "running": true, "already_running": false, "poll_seconds": poll_seconds }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_worker_start_check(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    log_file: &str,
) -> anyhow::Result<()> {
    let use_server_control_plane = crate::config::server_control_plane_enabled();
    let has_server_session = {
        let guard = state.lock().await;
        guard
            .session_token
            .as_deref()
            .map(str::trim)
            .is_some_and(|token| !token.is_empty())
    };
    if use_server_control_plane && !has_server_session {
        return write_session_required(socket).await;
    }

    let preflight = match collect_worker_start_preflight_bounded(state, log_file).await? {
        PreflightOutcome::Ready(preflight) => preflight,
        PreflightOutcome::TimedOut { seconds } => {
            return write_preflight_timeout_response(socket, seconds).await;
        }
    };
    let payload = json!({
        "success": true,
        "data": preflight,
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

fn parse_worker_start_request(body: &[u8]) -> WorkerStartRequest {
    serde_json::from_slice::<WorkerStartRequest>(body).unwrap_or_default()
}

enum PreflightOutcome {
    Ready(WorkerStartPreflightData),
    TimedOut { seconds: u64 },
}

/// Bound the whole worker-start preflight (component registry load, domain
/// list, per-site relations/rules fetches). Without this, a stalled component
/// catalog or WP endpoint can wedge /api/worker/start-check indefinitely
/// (observed with malformed local components awaiting a template alias
/// download from an unresponsive server).
/// WPTSALL_PREFLIGHT_TIMEOUT_SECS (default 60, 0 = unlimited) caps the total.
async fn collect_worker_start_preflight_bounded(
    state: &Arc<Mutex<WebUiState>>,
    log_file: &str,
) -> anyhow::Result<PreflightOutcome> {
    let seconds = env_u64("WPTSALL_PREFLIGHT_TIMEOUT_SECS", 60);
    if seconds == 0 {
        return Ok(PreflightOutcome::Ready(
            collect_worker_start_preflight(state, log_file).await?,
        ));
    }
    match tokio::time::timeout(
        Duration::from_secs(seconds),
        collect_worker_start_preflight(state, log_file),
    )
    .await
    {
        Ok(result) => Ok(PreflightOutcome::Ready(result?)),
        Err(_) => {
            let _ = log_event(
                log_file,
                "warning",
                "worker.preflight.timeout",
                json!({ "timeout_seconds": seconds }),
            );
            Ok(PreflightOutcome::TimedOut { seconds })
        }
    }
}

async fn write_preflight_timeout_response(
    socket: &mut TcpStream,
    seconds: u64,
) -> anyhow::Result<()> {
    write_error_response_with_status(
        socket,
        "504 Gateway Timeout",
        "WORKER_START_PREFLIGHT_TIMEOUT",
        &format!(
            "worker preflight exceeded {}s: component catalog or site relation fetch stalled; \
             check network, WP endpoints, or local component configuration",
            seconds
        ),
    )
    .await
}

async fn collect_worker_start_preflight(
    state: &Arc<Mutex<WebUiState>>,
    log_file: &str,
) -> anyhow::Result<WorkerStartPreflightData> {
    let use_server_control_plane = crate::config::server_control_plane_enabled();
    let (
        server_base,
        device_id,
        session_token,
        domain_token_bindings,
        component_bindings_path,
        db_arc,
        client,
        component_bindings,
        task_type_component_bindings,
        rule_component_bindings,
    ) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.device_id.clone(),
            guard.session_token.clone(),
            guard.domain_token_bindings.clone(),
            guard.component_bindings_path.clone(),
            std::sync::Arc::clone(&guard.db),
            guard.http_client.clone(),
            guard.component_bindings.clone(),
            guard.task_type_component_bindings.clone(),
            guard.rule_component_bindings.clone(),
        )
    };

    let session_token = if use_server_control_plane {
        Some(
            session_token
                .filter(|token| !token.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("SESSION_REQUIRED: login required before worker preflight")
                })?,
        )
    } else {
        None
    };

    let component_runtime_enabled = crate::config::env_bool("WPTSALL_COMPONENT_RUNTIME", true);
    let component_registry: Option<Arc<crate::types::ComponentRuntimeRegistry>> =
        if component_runtime_enabled {
            let local_components_doc = crate::db::components::load_runtime_local_components_doc();
            let target_component_ids = collect_configured_runtime_component_ids(
                &local_components_doc,
                Some(&task_type_component_bindings),
                Some(&rule_component_bindings),
                Some(&component_bindings),
            );
            let signing_key_from_db = {
                let conn = db_arc.lock().await;
                crate::db::system::get_signing_key(&conn)
                    .filter(|pem| pem.trim().starts_with("-----BEGIN PUBLIC KEY-----"))
            };
            let session_token_str = session_token.as_deref().unwrap_or("");
            // A stalled component catalog (e.g. a local component that needs a
            // template alias download from an unresponsive server) must not
            // wedge the preflight forever: bound the registry load and degrade
            // exactly like a load error. WPTSALL_PREFLIGHT_COMPONENT_TIMEOUT_SECS
            // (default 15, 0 = unlimited) caps this phase; the total preflight
            // bound (WPTSALL_PREFLIGHT_TIMEOUT_SECS) still applies on top.
            let component_timeout_seconds =
                env_u64("WPTSALL_PREFLIGHT_COMPONENT_TIMEOUT_SECS", 15);
            let mut component_bindings_for_load = component_bindings.clone();
            let registry_load = load_component_runtimes(
                &client,
                &server_base,
                session_token_str,
                log_file,
                &mut component_bindings_for_load,
                &component_bindings_path,
                Some(&target_component_ids),
                signing_key_from_db.as_deref(),
            );
            let timed_registry_load = match component_timeout_seconds {
                0 => tokio::time::timeout(Duration::MAX, registry_load),
                secs => tokio::time::timeout(Duration::from_secs(secs), registry_load),
            };
            match timed_registry_load.await {
                Ok(Ok(registry)) => Some(Arc::new(registry)),
                Ok(Err(err)) => {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "worker.preflight.component_registry_unavailable",
                        json!({ "error": snippet(&format!("{:#}", err)) }),
                    );
                    None
                }
                Err(_) => {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "worker.preflight.component_registry_timeout",
                        json!({ "timeout_seconds": component_timeout_seconds }),
                    );
                    None
                }
            }
        } else {
            None
        };

    if component_registry.is_none() {
        return Ok(WorkerStartPreflightData {
            can_start: true,
            requires_confirmation: false,
            summary: WorkerStartPreflightSummary::default(),
            missing_components: Vec::new(),
        });
    }

    let worker_id = crate::worker::build_worker_config(&device_id).worker_id;
    let wp_client_token_fallback = if use_server_control_plane {
        crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "")
    } else {
        String::new()
    };
    let domains = if use_server_control_plane {
        let session_token = session_token.as_deref().unwrap_or("");
        fetch_domains_for_session(&client, &server_base, session_token).await?
    } else {
        local_sites_from_domain_token_bindings(&domain_token_bindings)
    };
    let occupied_domain_bases: HashSet<String> = domains
        .iter()
        .filter(|d| d.site_status != LOCAL_DEV_LICENSE_STATUS)
        .map(|d| crate::bindings::normalize_domain_base(&d.api_base_url))
        .filter(|v| !v.is_empty())
        .collect();

    let mut summary = WorkerStartPreflightSummary::default();
    let mut missing_components = Vec::new();
    let registry = component_registry.as_ref().unwrap();

    for domain in &domains {
        let is_local_dev = domain.site_status == LOCAL_DEV_LICENSE_STATUS;
        // Free users still proceed — do NOT skip non-active domains.

        let (domain_api_base, wp_client_token, route_secret) = if is_local_dev {
            let Some((local_base, token, secret)) = crate::bindings::resolve_local_dev_binding(
                &domain_token_bindings,
                &occupied_domain_bases,
            ) else {
                continue;
            };
            (local_base, token, secret)
        } else {
            let Some(token) = crate::bindings::resolve_wp_client_token_for_domain(
                &domain.api_base_url,
                &domain_token_bindings,
                &wp_client_token_fallback,
            ) else {
                continue;
            };
            let route_secret = crate::bindings::resolve_route_secret_for_domain(
                &domain.api_base_url,
                &domain_token_bindings,
            )
            .or_else(|| domain.route_secret.clone());
            (domain.api_base_url.clone(), token, route_secret)
        };

        let domain_base = crate::bindings::normalize_domain_base(&domain_api_base);
        let Some(wp_base) = route_secret
            .as_deref()
            .and_then(|secret| crate::bindings::build_wp_base_url(&domain_base, secret))
        else {
            continue;
        };

        let relations_url = format!("{}/site-relations", wp_base);
        let relations = match crate::auth::wp_get_json_with_transport_and_secret::<
            crate::types::RelationsResponse,
        >(
            &client,
            &relations_url,
            &wp_client_token,
            &worker_id,
            route_secret.as_deref(),
        )
        .await
        {
            Ok(relations) => relations,
            Err(err) => {
                let _ = log_event(
                    log_file,
                    "warning",
                    "worker.preflight.fetch_relations_failed",
                    json!({
                        "api_base_url": domain_api_base,
                        "error": snippet(&format!("{:#}", err)),
                    }),
                );
                continue;
            }
        };

        summary.domains_checked += 1;

        for relation in relations.relations {
            summary.relations_checked += 1;
            let rules_url = format!("{}/rules?relation_id={}", wp_base, relation.id);
            let rules = match crate::auth::wp_get_json_with_transport_and_secret::<
                crate::types::RulesResponse,
            >(
                &client,
                &rules_url,
                &wp_client_token,
                &worker_id,
                route_secret.as_deref(),
            )
            .await
            {
                Ok(rules) => rules.rules,
                Err(err) => {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "worker.preflight.fetch_rules_failed",
                        json!({
                            "api_base_url": domain_api_base,
                            "relation_id": relation.id,
                            "error": snippet(&format!("{:#}", err)),
                        }),
                    );
                    continue;
                }
            };

            for business_line in enabled_i18n_business_lines(&relation) {
                summary.language_pack_lanes_checked += 1;
                let payload = json!({
                    "__input_artifact_kind": "i18n_bundle",
                    "__expected_output_artifact_kind": "translated_i18n_bundle"
                });
                let selected =
                    crate::component_rt::selector::select_component_runtime_for_task_type(
                        Some(registry.as_ref()),
                        &payload,
                        business_line,
                        "text",
                        "",
                        &[],
                        Some(&task_type_component_bindings),
                    );
                if selected.is_none() {
                    let (preflight_policy, missing_component_behavior, severity) =
                        preflight_missing_component_behavior(&relation);
                    missing_components.push(WorkerStartMissingComponent {
                        api_base_url: domain_api_base.clone(),
                        business_line: business_line.to_string(),
                        relation_id: relation.id,
                        rule_id: None,
                        source_group: "i18n_bundle".to_string(),
                        routing_profile: business_line.to_string(),
                        delivery_target: "i18n_deploy".to_string(),
                        object_name: "language_pack".to_string(),
                        field_name: "entries[*].msgid/msgid_plural".to_string(),
                        source_role: "gettext_entry".to_string(),
                        preflight_policy,
                        missing_component_behavior,
                        severity: severity.to_string(),
                        content_format: "plain_text".to_string(),
                        required_slot_key: "plain_text".to_string(),
                        suggested_task_type: "text".to_string(),
                        input_artifact_kind: Some("i18n_bundle".to_string()),
                        expected_output_artifact_kind: Some("translated_i18n_bundle".to_string()),
                    });
                }
            }

            for rule in &rules {
                summary.rules_checked += 1;
                let translate_fields = if !rule.translate_fields.is_empty() {
                    rule.translate_fields.clone()
                } else {
                    crate::task_engine::pipeline::extract_translate_fields(&rule.field_capabilities)
                };
                let plugin_slug = resolve_rule_plugin_slug(&relation, rule);
                let rule_id = u64::try_from(rule.id).ok();
                let relation_id = u64::try_from(relation.id).ok();
                let business_line = preflight_business_line_for_rule(rule);

                for field_name in translate_fields {
                    let raw_content_format =
                        resolve_preflight_field_content_format(rule, &field_name)
                            .unwrap_or_else(|| "plain_text".to_string());
                    let content_format = normalize_preflight_content_format(&raw_content_format);
                    if content_format == "code" {
                        continue;
                    }
                    summary.fields_checked += 1;

                    let source_role = resolve_preflight_field_source_role(rule, &field_name);
                    let selection =
                        selection_requirements_for_field(rule, &field_name, &content_format);
                    let selected =
                        crate::component_rt::selector::select_component_with_format_awareness(
                            Some(registry.as_ref()),
                            Some(&rule_component_bindings),
                            rule_id,
                            relation_id,
                            plugin_slug.as_deref(),
                            &selection.payload,
                            business_line,
                            selection.task_type,
                            &selection.routing_content_format,
                            "",
                            &[],
                            Some(&task_type_component_bindings),
                        );
                    if selected.is_none() {
                        let (preflight_policy, missing_component_behavior, severity) =
                            preflight_missing_component_behavior(&relation);
                        missing_components.push(WorkerStartMissingComponent {
                            api_base_url: domain_api_base.clone(),
                            business_line: business_line.to_string(),
                            relation_id: relation.id,
                            rule_id: Some(rule.id),
                            source_group: normalize_preflight_label(&rule.source_group),
                            routing_profile: normalize_preflight_label(&rule.routing_profile),
                            delivery_target: normalize_preflight_label(&rule.delivery_target),
                            object_name: rule.object_name.clone(),
                            field_name,
                            source_role,
                            preflight_policy,
                            missing_component_behavior,
                            severity: severity.to_string(),
                            content_format,
                            required_slot_key: selection.required_slot_key,
                            suggested_task_type: selection.task_type.to_string(),
                            input_artifact_kind: selection.input_artifact_kind,
                            expected_output_artifact_kind: selection.expected_output_artifact_kind,
                        });
                    }
                }
            }
        }
    }

    for item in &missing_components {
        match item.severity.as_str() {
            "blocking" => summary.blocking_missing_components += 1,
            "auto_skip" => summary.auto_skip_missing_components += 1,
            _ => summary.confirm_missing_components += 1,
        }
    }

    Ok(WorkerStartPreflightData {
        can_start: summary.blocking_missing_components == 0,
        requires_confirmation: summary.confirm_missing_components > 0,
        summary,
        missing_components,
    })
}

fn enabled_i18n_business_lines(relation: &crate::types::DiscoveredRelation) -> Vec<&'static str> {
    let Some(i18n_config) = relation.i18n_config.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if i18n_config.translate_plugin_i18n {
        out.push("plugin_i18n");
    }
    if i18n_config.translate_theme_i18n {
        out.push("theme_i18n");
    }
    if i18n_config.translate_config_i18n {
        out.push("config_i18n");
    }
    if i18n_config.translate_site_strings {
        out.push("site_strings");
    }
    if i18n_config.translate_menu_strings {
        out.push("menu_strings");
    }
    if i18n_config.translate_widget_strings {
        out.push("widget_strings");
    }
    out
}

fn resolve_rule_plugin_slug(
    relation: &crate::types::DiscoveredRelation,
    rule: &crate::types::DiscoveredRule,
) -> Option<String> {
    relation
        .models
        .iter()
        .find(|model| model.model_id == rule.model_id)
        .map(|model| model.plugin_slug.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn preflight_business_line_for_rule(
    rule: &crate::types::DiscoveredRule,
) -> &'static str {
    crate::task_engine::pipeline::derive_business_line_from_rule(Some(rule), &rule.data_type)
}

pub(crate) fn resolve_preflight_field_content_format(
    rule: &crate::types::DiscoveredRule,
    field_name: &str,
) -> Option<String> {
    if let Some(value) = rule.field_content_formats.get(field_name) {
        return Some(value.clone());
    }
    let caps = rule.field_capabilities.as_object()?;
    let field_cfg = caps.get(field_name)?.as_object()?;
    field_cfg
        .get("content_format")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

pub(crate) fn resolve_preflight_field_source_role(
    rule: &crate::types::DiscoveredRule,
    field_name: &str,
) -> String {
    rule.field_source_roles
        .get(field_name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "body".to_string())
}

pub(crate) fn normalize_preflight_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "-".to_string()
    } else {
        trimmed.to_string()
    }
}

fn preflight_missing_component_behavior(
    relation: &crate::types::DiscoveredRelation,
) -> (String, String, &'static str) {
    let policy = relation.preflight_policy.trim();
    let behavior = relation.missing_component_behavior.trim();

    let normalized_policy = if policy.is_empty() {
        "warn".to_string()
    } else {
        policy.to_string()
    };
    let normalized_behavior = if behavior.is_empty() {
        "confirm_continue".to_string()
    } else {
        behavior.to_string()
    };

    let severity = if normalized_policy.eq_ignore_ascii_case("block")
        || normalized_behavior.eq_ignore_ascii_case("stop_task")
    {
        "blocking"
    } else if normalized_behavior.eq_ignore_ascii_case("skip_unbound_fields") {
        "auto_skip"
    } else {
        "confirm"
    };

    (normalized_policy, normalized_behavior, severity)
}

pub(crate) fn normalize_preflight_content_format(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "rich_html" | "html" => "rich_html".to_string(),
        "json_structured" | "json" => "json_structured".to_string(),
        "serialized_php" | "serialized" => "serialized_php".to_string(),
        "media_ref" | "media" | "mediaref" => "media_ref".to_string(),
        "slug" => "slug".to_string(),
        "code" => "code".to_string(),
        _ => "plain_text".to_string(),
    }
}

pub(crate) struct PreflightSelection {
    pub(crate) task_type: &'static str,
    pub(crate) routing_content_format: String,
    pub(crate) required_slot_key: String,
    pub(crate) payload: Value,
    pub(crate) input_artifact_kind: Option<String>,
    pub(crate) expected_output_artifact_kind: Option<String>,
}

pub(crate) fn selection_requirements_for_field(
    rule: &crate::types::DiscoveredRule,
    field_name: &str,
    content_format: &str,
) -> PreflightSelection {
    let source_role = resolve_preflight_field_source_role(rule, field_name);
    if content_format == "media_ref" && !preflight_is_media_text_field(field_name, &source_role) {
        let task_type = infer_media_task_type_from_field_name(field_name);
        let (input_artifact_kind, expected_output_artifact_kind) =
            media_artifact_hints_for_task_type(task_type);
        let mut payload = json!({});
        if let Some(value) = input_artifact_kind {
            payload["__input_artifact_kind"] = Value::String(value.to_string());
        }
        if let Some(value) = expected_output_artifact_kind {
            payload["__expected_output_artifact_kind"] = Value::String(value.to_string());
        }
        return PreflightSelection {
            task_type,
            routing_content_format: "media_ref".to_string(),
            required_slot_key: format!("media_ref:{task_type}"),
            payload,
            input_artifact_kind: input_artifact_kind.map(str::to_string),
            expected_output_artifact_kind: expected_output_artifact_kind.map(str::to_string),
        };
    }

    if content_format == "slug" {
        return PreflightSelection {
            task_type: "text",
            routing_content_format: "plain_text".to_string(),
            required_slot_key: "plain_text".to_string(),
            payload: json!({}),
            input_artifact_kind: None,
            expected_output_artifact_kind: None,
        };
    }

    PreflightSelection {
        task_type: "text",
        routing_content_format: if content_format == "media_ref" {
            "plain_text".to_string()
        } else {
            content_format.to_string()
        },
        required_slot_key: if content_format == "media_ref" {
            "plain_text".to_string()
        } else {
            content_format.to_string()
        },
        payload: json!({}),
        input_artifact_kind: None,
        expected_output_artifact_kind: None,
    }
}

fn preflight_is_media_text_field(field_name: &str, source_role: &str) -> bool {
    if source_role.trim().eq_ignore_ascii_case("media_text") {
        return true;
    }
    let lower = field_name.to_ascii_lowercase();
    lower.contains("alt")
        || lower.contains("caption")
        || lower.contains("title")
        || lower.contains("description")
        || lower.contains("excerpt")
        || lower == "post_content"
}

fn infer_media_task_type_from_field_name(field_name: &str) -> &'static str {
    let lower = field_name.to_ascii_lowercase();
    if lower.contains("video") || lower.contains("movie") || lower.contains("subtitle") {
        return "video";
    }
    if lower.contains("audio") || lower.contains("sound") || lower.contains("voice") {
        return "audio";
    }
    if lower.contains("document")
        || lower.contains("file")
        || lower.contains("pdf")
        || lower.contains("doc")
        || lower.contains("sheet")
        || lower.contains("ppt")
    {
        return "document";
    }
    "image"
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_rule() -> crate::types::DiscoveredRule {
        crate::types::DiscoveredRule {
            id: 1,
            model_id: 1,
            name: "rule".to_string(),
            data_type: "post".to_string(),
            object_name: "post".to_string(),
            field_capabilities: json!({}),
            translate_fields: Vec::new(),
            related_taxonomies: Vec::new(),
            field_content_formats: HashMap::new(),
            field_storage_map: HashMap::new(),
            source_group: String::new(),
            routing_profile: String::new(),
            delivery_target: String::new(),
            required_component_slots: Vec::new(),
            required_content_formats: Vec::new(),
            field_source_roles: HashMap::new(),
        }
    }

    #[test]
    fn preflight_business_line_uses_custom_model_for_non_post_non_taxonomy_rules() {
        let mut rule = empty_rule();
        rule.data_type = "custom_model".to_string();
        assert_eq!(preflight_business_line_for_rule(&rule), "custom_model");
    }

    #[test]
    fn preflight_business_line_uses_config_i18n_for_config_object_rules() {
        let mut rule = empty_rule();
        rule.data_type = "option".to_string();
        rule.source_group = "config_object".to_string();
        rule.routing_profile = "config_i18n".to_string();
        assert_eq!(preflight_business_line_for_rule(&rule), "config_i18n");
    }

    #[test]
    fn media_text_source_role_stays_on_text_lane() {
        let mut rule = empty_rule();
        rule.field_source_roles.insert(
            "_wp_attachment_image_alt".to_string(),
            "media_text".to_string(),
        );

        let selection =
            selection_requirements_for_field(&rule, "_wp_attachment_image_alt", "media_ref");

        assert_eq!(selection.task_type, "text");
        assert_eq!(selection.required_slot_key, "plain_text");
        assert!(selection.input_artifact_kind.is_none());
    }

    #[test]
    fn media_file_field_uses_media_lane_and_artifact_hints() {
        let rule = empty_rule();
        let selection =
            selection_requirements_for_field(&rule, "_wptsall_core_source_file_id", "media_ref");

        assert_eq!(selection.task_type, "document");
        assert_eq!(selection.required_slot_key, "media_ref:document");
        assert_eq!(
            selection.input_artifact_kind.as_deref(),
            Some("document_file")
        );
        assert_eq!(
            selection.expected_output_artifact_kind.as_deref(),
            Some("translated_document_file")
        );
    }

    #[test]
    fn relation_preflight_policy_can_block_worker_start() {
        let relation = crate::types::DiscoveredRelation {
            id: 1,
            source_site_id: json!(1),
            source_lang: "en".to_string(),
            target_site_id: json!(2),
            target_site_type: "virtual".to_string(),
            target_lang: "zh".to_string(),
            sync_mode: "new_only".to_string(),
            media_handling: String::new(),
            template: String::new(),
            models: Vec::new(),
            i18n_config: None,
            source_group_config: None,
            preflight_policy: "block".to_string(),
            missing_component_behavior: "confirm_continue".to_string(),
        };

        let (policy, behavior, severity) = preflight_missing_component_behavior(&relation);
        assert_eq!(policy, "block");
        assert_eq!(behavior, "confirm_continue");
        assert_eq!(severity, "blocking");
    }
}

async fn spawn_worker_loop_task(
    loop_state: Arc<Mutex<WebUiState>>,
    loop_runtime: WebUiRuntimeControl,
    loop_log_file: String,
    poll_seconds: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _ = log_event(
            &loop_log_file,
            "info",
            "worker.loop.started",
            json!({ "poll_seconds": poll_seconds }),
        );
        loop {
            if !loop_runtime.worker_running.load(Ordering::SeqCst) {
                break;
            }
            match web_ui_run_worker_once(&loop_state, &loop_log_file).await {
                Err(err) => {
                    let err_text = format!("{:#}", err);
                    let _ = log_event(
                        &loop_log_file,
                        "warning",
                        "worker.loop.tick_failed",
                        json!({ "error": err_text }),
                    );
                    if is_auth_error_message(&err_text) {
                        {
                            let mut guard = loop_state.lock().await;
                            guard.last_error = err_text.clone();
                            guard.last_event = "worker.loop.auth_failed".to_string();
                            guard.worker_status = "error".to_string();
                            guard.updated_at = unix_ts();
                        }
                        loop_runtime.worker_running.store(false, Ordering::SeqCst);
                        let _ = log_event(
                            &loop_log_file,
                            "warning",
                            "worker.loop.stopping_auth_error",
                            json!({ "error": err_text }),
                        );
                    }
                }
                Ok(summary) => {
                    let total = summary
                        .get("total_items")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let _ = log_event(
                        &loop_log_file,
                        "info",
                        "worker.loop.tick_ok",
                        json!({ "total_items": total }),
                    );
                    if total == 0 {
                        let mut guard = loop_state.lock().await;
                        if guard.worker_status == "running_auto" {
                            guard.worker_status = "waiting".to_string();
                        }
                    } else {
                        let mut guard = loop_state.lock().await;
                        guard.worker_status = "running_auto".to_string();
                    }
                }
            }
            if !loop_runtime.worker_running.load(Ordering::SeqCst) {
                break;
            }
            let sleep_secs = {
                let guard = loop_state.lock().await;
                guard.worker_loop_poll_seconds.max(1)
            };
            tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
        }
        loop_runtime.worker_running.store(false, Ordering::SeqCst);
        {
            let mut guard = loop_state.lock().await;
            guard.worker_loop_running = false;
            guard.worker_status = "idle".to_string();
            guard.updated_at = unix_ts();
            if guard.last_event != "worker.loop.auth_failed"
                && guard.last_event != "worker.loop.stopped"
            {
                guard.last_event = "worker.loop.stopped".to_string();
            }
        }
        let mut handle_guard = loop_runtime.worker_handle.lock().await;
        *handle_guard = None;
        let _ = log_event(&loop_log_file, "info", "worker.loop.stopped", json!({}));
    })
}

pub(crate) async fn spawn_worker_loop(
    state: Arc<Mutex<WebUiState>>,
    runtime_control: WebUiRuntimeControl,
    log_file: String,
    _triggered_by: &str,
) {
    if runtime_control.worker_running.load(Ordering::SeqCst) {
        return;
    }
    runtime_control.worker_running.store(true, Ordering::SeqCst);
    let poll_seconds = {
        let mut guard = state.lock().await;
        guard.worker_loop_running = true;
        guard.worker_status = "running_auto".to_string();
        guard.last_event = "worker.loop.started".to_string();
        guard.updated_at = unix_ts();
        guard.worker_loop_poll_seconds
    };
    let join = spawn_worker_loop_task(
        Arc::clone(&state),
        runtime_control.clone(),
        log_file,
        poll_seconds,
    )
    .await;
    let mut handle_guard = runtime_control.worker_handle.lock().await;
    *handle_guard = Some(join);
}

pub(super) async fn handle_worker_config_get(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let (
        auto_start,
        domain_concurrency,
        relation_concurrency,
        global_translation_concurrency,
        global_callback_concurrency,
        relation_max_pending_callbacks,
        adaptive_rate_control,
        adaptive_max_delay_ms,
        cb_concurrency,
        cb_timeout,
        cb_retry,
        fetch_timeout,
        fetch_retry,
        review_mode,
        workflow_policy,
        workflow_dsl,
    ) = {
        let defaults = crate::resource_governor::ResourceGovernor::from_env();
        let conn = db_arc.lock().await;
        let auto_start = crate::db::system::get_system_config(&conn, "auto_start_worker")
            .map(|v| v == "true")
            .unwrap_or(false);
        let domain_concurrency = crate::db::system::get_system_config(&conn, "domain_concurrency")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(defaults.domain_concurrency as u64)
            .max(1);
        let relation_concurrency =
            crate::db::system::get_system_config(&conn, "relation_concurrency")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1)
                .max(1);
        let global_translation_concurrency =
            crate::db::system::get_system_config(&conn, "global_translation_concurrency")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(defaults.global_translation_concurrency as u64)
                .max(1);
        let global_callback_concurrency =
            crate::db::system::get_system_config(&conn, "global_callback_concurrency")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(defaults.global_callback_concurrency as u64)
                .max(1);
        let relation_max_pending_callbacks =
            crate::db::system::get_system_config(&conn, "relation_max_pending_callbacks")
                .and_then(|v| v.parse::<u64>().ok())
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
        let cb_concurrency = crate::db::system::get_system_config(&conn, "callback_concurrency")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(4);
        let cb_timeout = crate::db::system::get_system_config(&conn, "callback_timeout_secs")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);
        let cb_retry = crate::db::system::get_system_config(&conn, "callback_retry_max")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2);
        let fetch_timeout = crate::db::system::get_system_config(&conn, "fetch_timeout_secs")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20);
        let fetch_retry = crate::db::system::get_system_config(&conn, "fetch_retry_max")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2);
        let review_mode = crate::db::system::get_system_config(&conn, "review_mode")
            .map(|v| v == "true")
            .unwrap_or(false);
        let workflow_policy =
            crate::task_engine::workflow_policy::load_workflow_policy(&conn, review_mode);
        let workflow_dsl = crate::task_engine::workflow_dsl::load_workflow_dsl(&conn);
        (
            auto_start,
            domain_concurrency,
            relation_concurrency,
            global_translation_concurrency,
            global_callback_concurrency,
            relation_max_pending_callbacks,
            adaptive_rate_control,
            adaptive_max_delay_ms,
            cb_concurrency,
            cb_timeout,
            cb_retry,
            fetch_timeout,
            fetch_retry,
            review_mode,
            workflow_policy,
            workflow_dsl,
        )
    };
    let poll = {
        let guard = state.lock().await;
        guard.worker_loop_poll_seconds
    };
    let payload = json!({
        "success": true,
        "data": {
            "poll_seconds": poll,
            "auto_start_worker": auto_start,
            "domain_concurrency": domain_concurrency,
            "relation_concurrency": relation_concurrency,
            "global_translation_concurrency": global_translation_concurrency,
            "global_callback_concurrency": global_callback_concurrency,
            "relation_max_pending_callbacks": relation_max_pending_callbacks,
            "adaptive_rate_control": adaptive_rate_control,
            "adaptive_max_delay_ms": adaptive_max_delay_ms,
            "callback_concurrency": cb_concurrency,
            "callback_timeout_secs": cb_timeout,
            "callback_retry_max": cb_retry,
            "fetch_timeout_secs": fetch_timeout,
            "fetch_retry_max": fetch_retry,
            "review_mode": review_mode,
            "workflow_policy": workflow_policy,
            "workflow_dsl": workflow_dsl,
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

pub(super) async fn handle_worker_config(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiWorkerConfigRequest =
        serde_json::from_slice(body).with_context(|| "invalid /api/worker/config json payload")?;
    let poll = match req.poll_seconds {
        Some(poll) => poll.clamp(1, 3600),
        None => {
            let guard = state.lock().await;
            guard.worker_loop_poll_seconds.max(1)
        }
    };

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    {
        let conn = db_arc.lock().await;
        if let Some(v) = req.auto_start_worker {
            let _ = crate::db::system::set_system_config(
                &conn,
                "auto_start_worker",
                if v { "true" } else { "false" },
            );
        }
        if let Some(v) = req.domain_concurrency {
            let _ = crate::db::system::set_system_config(
                &conn,
                "domain_concurrency",
                &v.max(1).to_string(),
            );
        }
        if let Some(v) = req.relation_concurrency {
            let _ = crate::db::system::set_system_config(
                &conn,
                "relation_concurrency",
                &v.max(1).to_string(),
            );
        }
        if let Some(v) = req.global_translation_concurrency {
            let _ = crate::db::system::set_system_config(
                &conn,
                "global_translation_concurrency",
                &v.max(1).to_string(),
            );
        }
        if let Some(v) = req.global_callback_concurrency {
            let _ = crate::db::system::set_system_config(
                &conn,
                "global_callback_concurrency",
                &v.max(1).to_string(),
            );
        }
        if let Some(v) = req.relation_max_pending_callbacks {
            let _ = crate::db::system::set_system_config(
                &conn,
                "relation_max_pending_callbacks",
                &v.max(1).to_string(),
            );
        }
        if let Some(v) = req.adaptive_rate_control {
            let _ = crate::db::system::set_system_config(
                &conn,
                "adaptive_rate_control",
                if v { "true" } else { "false" },
            );
        }
        if let Some(v) = req.adaptive_max_delay_ms {
            let _ = crate::db::system::set_system_config(
                &conn,
                "adaptive_max_delay_ms",
                &v.max(200).to_string(),
            );
        }
        if let Some(v) = req.callback_concurrency {
            let _ = crate::db::system::set_system_config(
                &conn,
                "callback_concurrency",
                &v.max(1).to_string(),
            );
        }
        if let Some(v) = req.callback_timeout_secs {
            let _ = crate::db::system::set_system_config(
                &conn,
                "callback_timeout_secs",
                &v.to_string(),
            );
        }
        if let Some(v) = req.callback_retry_max {
            let _ =
                crate::db::system::set_system_config(&conn, "callback_retry_max", &v.to_string());
        }
        if let Some(v) = req.fetch_timeout_secs {
            let _ =
                crate::db::system::set_system_config(&conn, "fetch_timeout_secs", &v.to_string());
        }
        if let Some(v) = req.fetch_retry_max {
            let _ = crate::db::system::set_system_config(&conn, "fetch_retry_max", &v.to_string());
        }
        if let Some(v) = req.review_mode {
            let _ = crate::db::system::set_system_config(
                &conn,
                "review_mode",
                if v { "true" } else { "false" },
            );
        }
        if let Some(policy) = req.workflow_policy.as_ref() {
            if let Ok(raw) = serde_json::to_string(policy) {
                if crate::task_engine::workflow_policy::WorkflowPolicy::parse_json(&raw).is_some() {
                    let _ = crate::db::system::set_system_config(&conn, "workflow_policy", &raw);
                }
            }
        }
        if let Some(dsl) = req.workflow_dsl.as_ref() {
            if let Ok(raw) = serde_json::to_string(dsl) {
                if crate::task_engine::workflow_dsl::WorkflowDsl::parse_json(&raw).is_some() {
                    let _ = crate::db::system::set_system_config(&conn, "workflow_dsl", &raw);
                }
            }
        }
    }

    {
        let mut guard = state.lock().await;
        guard.worker_loop_poll_seconds = poll;
        {
            let conn = db_arc.lock().await;
            let _ = crate::db::system::set_system_config(
                &conn,
                "worker_poll_seconds",
                &poll.to_string(),
            );
        }
        guard.last_error.clear();
        guard.last_event = "worker.loop.config_updated".to_string();
        guard.updated_at = unix_ts();
    }
    let payload = json!({
        "success": true,
        "data": { "poll_seconds": poll }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_worker_stop(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    runtime_control: &WebUiRuntimeControl,
) -> anyhow::Result<()> {
    runtime_control
        .worker_running
        .store(false, Ordering::SeqCst);
    let handle_opt = {
        let mut handle_guard = runtime_control.worker_handle.lock().await;
        handle_guard.take()
    };
    if let Some(handle) = handle_opt {
        handle.abort();
        let _ = handle.await;
    }
    {
        let mut guard = state.lock().await;
        guard.worker_loop_running = false;
        guard.worker_status = "idle".to_string();
        guard.last_event = "worker.loop.stopped".to_string();
        guard.updated_at = unix_ts();
    }
    let payload = json!({
        "success": true,
        "data": { "running": false }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}
