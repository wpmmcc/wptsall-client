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
use crate::logging::{flush_log, log_event, snippet, unix_ts};
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
    /// Configured domains the preflight could not check at all (no client
    /// token, no route secret, no resolvable WP base, or the relations fetch
    /// failed). §63 (tasks/cursor feedback): with every configured domain
    /// skipped the old response was a vacuous green light — can_start:true
    /// with domains_checked:0 and zero signal that nothing was verifiable.
    domains_skipped: usize,
    /// The component registry could not be loaded (load error or timeout),
    /// so no domain/component checks could run at all. §64 recheck: surfaced
    /// with requires_confirmation instead of a vacuous green light.
    component_registry_unavailable: bool,
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
pub(super) struct WorkerStartPreflightData {
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

/// 批D (X-12④) fast-fail brake: a run-once loop whose iterations keep
/// processing items but NEVER succeeding (total > 0, succeeded == 0,
/// failed > 0) used to match none of the break conditions and spin to the
/// 180-iteration / 900-second cap burning CPU (observed: 423% CPU, zero
/// results, 10 minutes — the client-side twin of the WP-side reverse-FIFO
/// starvation fixed in X-12①). Two CONSECUTIVE all-fail iterations trip
/// the brake: one alone can be a transient blip, and the WP-side
/// available_at backoff usually makes the next iteration claim nothing
/// (the empty_queue break then stops the loop naturally). Any iteration
/// with progress (or a clean noop/empty) resets the ladder.
struct ZeroSuccessBrake {
    consecutive_all_fail: u32,
}

impl ZeroSuccessBrake {
    fn new() -> Self {
        Self {
            consecutive_all_fail: 0,
        }
    }

    /// Record one iteration's outcome; Some(reason) when the brake trips.
    fn record(&mut self, total: i64, succeeded: i64, failed: i64) -> Option<&'static str> {
        if total > 0 && succeeded == 0 && failed > 0 {
            self.consecutive_all_fail += 1;
            if self.consecutive_all_fail >= 2 {
                return Some("zero_success_all_failing");
            }
        } else {
            self.consecutive_all_fail = 0;
        }
        None
    }
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
    let mut zero_success_brake = ZeroSuccessBrake::new();

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
                        "elapsed_ms": started_at.elapsed().as_millis() as u64,
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
                // 批D (X-12④): consecutive all-fail iterations trip the
                // fast-fail brake instead of spinning to the iteration /
                // elapsed caps with zero progress.
                if let Some(reason) = zero_success_brake.record(total, succeeded, failed) {
                    break_reason = Some(reason);
                    let _ = log_event(
                        log_file,
                        "warning",
                        "worker.run_once.break",
                        json!({
                            "reason": reason,
                            "iteration": iterations,
                            "consecutive_all_fail_iterations": zero_success_brake.consecutive_all_fail,
                            "tasks_processed": acc_processed,
                            "tasks_succeeded": acc_succeeded,
                            "tasks_failed": acc_failed
                        }),
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

    let _lifecycle = runtime_control.worker_lifecycle.lock().await;
    if !runtime_control.worker_running.load(Ordering::SeqCst) {
        stop_worker_loop_inner(runtime_control, Duration::from_secs(2)).await;
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

pub(super) async fn collect_worker_start_preflight(
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
            let local_components_doc = crate::db::components::load_runtime_local_components_doc()?;
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
            let component_timeout_seconds = env_u64("WPTSALL_PREFLIGHT_COMPONENT_TIMEOUT_SECS", 15);
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
                Ok(Err(err))
                    if err.is::<crate::component_rt::loader::RuntimeConfigurationFault>() =>
                {
                    return Err(err)
                }
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
        // §64 recheck (tasks/cursor): a registry that failed to load (or
        // timed out) must not produce the same vacuous green light the
        // all-domains-skipped branch used to — the domain/component checks
        // below cannot run at all. Surface the degraded state and require
        // explicit confirmation to start. The deliberately-disabled case
        // (WPTSALL_COMPONENT_RUNTIME=false) keeps its operator-configured
        // can_start:true without the degraded flag.
        let registry_load_failed = component_runtime_enabled;
        return Ok(WorkerStartPreflightData {
            can_start: true,
            requires_confirmation: registry_load_failed,
            summary: WorkerStartPreflightSummary {
                component_registry_unavailable: registry_load_failed,
                ..Default::default()
            },
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
                summary.domains_skipped += 1;
                continue;
            };
            (local_base, token, secret)
        } else {
            let Some(token) = crate::bindings::resolve_wp_client_token_for_domain(
                &domain.api_base_url,
                &domain_token_bindings,
                &wp_client_token_fallback,
            ) else {
                summary.domains_skipped += 1;
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
            summary.domains_skipped += 1;
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
            &device_id,
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
                summary.domains_skipped += 1;
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
                &device_id,
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
                        // FL-9: capability probe — keep unguarded.
                        None,
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
                let translate_fields =
                    crate::task_engine::field_action::plan_field_actions(rule).translate;
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
                            // FL-9: preflight is a capability probe, not
                            // task-scoped execution — keep it unguarded.
                            None,
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
        // §63: when every configured domain was skipped (none checkable at
        // all), do not hand back a bare green light — require confirmation so
        // the operator sees the preflight verified nothing instead of a
        // silent can_start:true over 0 checked / 0 missing.
        requires_confirmation: summary.confirm_missing_components > 0
            || (summary.domains_skipped > 0 && summary.domains_checked == 0),
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



async fn spawn_worker_loop_task(
    loop_state: Arc<Mutex<WebUiState>>,
    loop_runtime: WebUiRuntimeControl,
    loop_log_file: String,
    poll_seconds: u64,
) -> tokio::task::JoinHandle<()> {
    let loop_lease = {
        let db = { Arc::clone(&loop_state.lock().await.db) };
        let conn = db.lock().await;
        crate::db::runtime::RuntimeLease::for_connection(&conn)
    };
    tokio::spawn(async move {
        let _lease = loop_lease;
        let _ = log_event(
            &loop_log_file,
            "info",
            "worker.loop.started",
            json!({ "poll_seconds": poll_seconds }),
        );
        // Flush immediately: loop lifecycle events are info level (buffered
        // behind the 8 KB threshold by default) but external gates assert on
        // them while the client is still running — same rationale as the
        // warning-level identity events. Without this, a quiet loop can sit
        // with `worker.loop.started` invisible in the log file for minutes.
        let _ = flush_log();
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
                    // Flush immediately (see the started-event rationale):
                    // gates assert per-tick evidence while the client runs,
                    // and an empty-queue tick generates no further log lines
                    // to push the buffer past the 8 KB threshold.
                    let _ = flush_log();
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
            let stopped = loop_runtime.worker_stop.notified();
            tokio::pin!(stopped);
            stopped.as_mut().enable();
            if !loop_runtime.worker_running.load(Ordering::SeqCst) {
                break;
            }
            let sleep_secs = {
                let guard = loop_state.lock().await;
                guard.worker_loop_poll_seconds.max(1)
            };
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {}
                _ = &mut stopped => {}
            }
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
        // Flush immediately (see the started-event rationale): the stop-side
        // handler flushes its own copy, but this natural-tail event must also
        // reach external observers before any process exit can drop it.
        let _ = flush_log();
    })
}

pub(crate) async fn spawn_worker_loop(
    state: Arc<Mutex<WebUiState>>,
    runtime_control: WebUiRuntimeControl,
    log_file: String,
    _triggered_by: &str,
) {
    let _lifecycle = runtime_control.worker_lifecycle.lock().await;
    if runtime_control.worker_running.load(Ordering::SeqCst) {
        return;
    }
    stop_worker_loop_inner(&runtime_control, Duration::from_secs(2)).await;
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

fn worker_config_snapshot(conn: &rusqlite::Connection, poll: u64) -> anyhow::Result<Value> {
    let get = |key| crate::db::system::get_system_config_checked(conn, key);
    let number = |key, fallback, min, max| -> anyhow::Result<u64> {
        Ok(get(key)?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(fallback)
            .clamp(min, max))
    };
    let defaults = crate::resource_governor::ResourceGovernor::from_env();
    let review_mode_stored = get("review_mode")?.is_some_and(|v| v == "true");
    let workflow_policy = get("workflow_policy")?
        .and_then(|raw| crate::task_engine::workflow_policy::WorkflowPolicy::parse_json(&raw))
        .unwrap_or_else(|| {
            crate::task_engine::workflow_policy::WorkflowPolicy::from_review_mode_flag(
                review_mode_stored,
            )
        });
    let review_mode = workflow_policy.resolve(None, None, None)
        == crate::task_engine::workflow_policy::WorkflowMode::Review;
    let workflow_dsl = get("workflow_dsl")?
        .and_then(|raw| crate::task_engine::workflow_dsl::WorkflowDsl::parse_json(&raw))
        .unwrap_or_default();
    let capacity = crate::db::capacity::inventory(conn)?;
    Ok(json!({
        "poll_seconds": poll,
        "auto_start_worker": get("auto_start_worker")?.is_some_and(|v| v == "true"),
        "domain_concurrency": number("domain_concurrency", defaults.domain_concurrency as u64, 1, u64::MAX)?,
        "relation_concurrency": number("relation_concurrency", 1, 1, u64::MAX)?,
        "global_translation_concurrency": number("global_translation_concurrency", defaults.global_translation_concurrency as u64, 1, u64::MAX)?,
        "global_callback_concurrency": number("global_callback_concurrency", defaults.global_callback_concurrency as u64, 1, u64::MAX)?,
        "relation_max_pending_callbacks": number("relation_max_pending_callbacks", 200, 1, u64::MAX)?,
        "storage_max_retained_units": capacity.max_retained_units,
        "storage_max_reserved_bytes": capacity.max_reserved_bytes,
        "storage_capacity": capacity,
        "adaptive_rate_control": get("adaptive_rate_control")?.map(|v| v == "true").unwrap_or(true),
        "adaptive_max_delay_ms": number("adaptive_max_delay_ms", 5000, 200, u64::MAX)?,
        "callback_concurrency": number("callback_concurrency", 4, 1, 50)?,
        "callback_timeout_secs": number("callback_timeout_secs", 30, 1, 300)?,
        "callback_retry_max": number("callback_retry_max", 2, 0, 10)?,
        "fetch_timeout_secs": number("fetch_timeout_secs", 20, 1, 300)?,
        "fetch_retry_max": number("fetch_retry_max", 2, 0, 10)?,
        "review_mode": review_mode,
        "workflow_policy": workflow_policy,
        "workflow_dsl": workflow_dsl,
    }))
}

pub(super) async fn handle_worker_config_get(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let saved = {
        let guard = state.lock().await;
        let conn = guard.db.lock().await;
        worker_config_snapshot(&conn, guard.worker_loop_poll_seconds)
    };
    let data = match saved {
        Ok(data) => data,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "WORKER_CONFIG_READ_FAILED",
                &super::errors::err_public(&error),
            )
            .await;
        }
    };
    let payload = json!({ "success": true, "data": data });
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
    let req: WebUiWorkerConfigRequest = match serde_json::from_slice(body) {
        Ok(req) if matches!(serde_json::from_slice::<Value>(body), Ok(Value::Object(_))) => req,
        _ => {
            return write_error_response(
                socket,
                "INVALID_WORKER_CONFIG",
                "Invalid worker configuration JSON.",
            )
            .await
        }
    };
    if [
        req.storage_max_retained_units,
        req.storage_max_reserved_bytes,
    ]
    .into_iter()
    .flatten()
    .any(|value| !(1..=crate::db::capacity::MAX_CONFIG_LIMIT).contains(&value))
    {
        return write_error_response(
            socket,
            "INVALID_STORAGE_CAPACITY",
            "Retained capacity limits must be positive safe integers.",
        )
        .await;
    }
    // Validate the complete request before writing even its first field.
    let policy = if let Some(value) = req.workflow_policy.as_ref() {
        let raw = value.to_string();
        let Some(parsed) = crate::task_engine::workflow_policy::WorkflowPolicy::parse_json(&raw)
        else {
            return write_error_response(
                socket,
                "INVALID_WORKFLOW_POLICY",
                "Invalid workflow policy.",
            )
            .await;
        };
        let review = parsed.resolve(None, None, None)
            == crate::task_engine::workflow_policy::WorkflowMode::Review;
        Some((raw, review))
    } else {
        None
    };
    let dsl = if let Some(value) = req.workflow_dsl.as_ref() {
        let raw = value.to_string();
        if crate::task_engine::workflow_dsl::WorkflowDsl::parse_json(&raw).is_none() {
            return write_error_response(socket, "INVALID_WORKFLOW_DSL", "Invalid workflow DSL.")
                .await;
        }
        Some(raw)
    } else {
        None
    };
    // Keep DB and runtime changes in the same order across concurrent saves.
    let mut guard = state.lock().await;
    let poll = req
        .poll_seconds
        .unwrap_or(guard.worker_loop_poll_seconds)
        .clamp(1, 3600);
    let saved = {
        let mut conn = guard.db.lock().await;
        (|| -> anyhow::Result<Value> {
            let tx = conn.transaction()?;
            for (key, value) in [
                ("storage_max_retained_units", req.storage_max_retained_units),
                ("storage_max_reserved_bytes", req.storage_max_reserved_bytes),
            ] {
                if let Some(value) = value {
                    crate::db::system::set_system_config(&tx, key, &value.to_string())?;
                    anyhow::ensure!(
                        crate::db::system::get_system_config_checked(&tx, key)?.as_deref()
                            == Some(value.to_string().as_str()),
                        "retained capacity setting was not committed"
                    );
                }
            }
            for (key, value, min, max) in [
                ("domain_concurrency", req.domain_concurrency, 1, u64::MAX),
                (
                    "relation_concurrency",
                    req.relation_concurrency,
                    1,
                    u64::MAX,
                ),
                (
                    "global_translation_concurrency",
                    req.global_translation_concurrency,
                    1,
                    u64::MAX,
                ),
                (
                    "global_callback_concurrency",
                    req.global_callback_concurrency,
                    1,
                    u64::MAX,
                ),
                (
                    "relation_max_pending_callbacks",
                    req.relation_max_pending_callbacks,
                    1,
                    u64::MAX,
                ),
                (
                    "adaptive_max_delay_ms",
                    req.adaptive_max_delay_ms,
                    200,
                    u64::MAX,
                ),
                ("callback_concurrency", req.callback_concurrency, 1, 50),
                ("callback_timeout_secs", req.callback_timeout_secs, 1, 300),
                ("callback_retry_max", req.callback_retry_max, 0, 10),
                ("fetch_timeout_secs", req.fetch_timeout_secs, 1, 300),
                ("fetch_retry_max", req.fetch_retry_max, 0, 10),
            ] {
                if let Some(value) = value {
                    crate::db::system::set_system_config(
                        &tx,
                        key,
                        &value.clamp(min, max).to_string(),
                    )?;
                }
            }
            for (key, value) in [
                ("auto_start_worker", req.auto_start_worker),
                ("adaptive_rate_control", req.adaptive_rate_control),
            ] {
                if let Some(value) = value {
                    crate::db::system::set_system_config(
                        &tx,
                        key,
                        if value { "true" } else { "false" },
                    )?;
                }
            }
            // An explicit policy owns default_mode; a flag-only save updates
            // that default while preserving its scoped overrides (FL-8).
            if let Some((raw, review)) = &policy {
                crate::db::system::set_system_config(&tx, "workflow_policy", raw)?;
                crate::db::system::set_system_config(
                    &tx,
                    "review_mode",
                    if *review { "true" } else { "false" },
                )?;
            } else if let Some(review) = req.review_mode {
                crate::db::system::set_system_config(
                    &tx,
                    "review_mode",
                    if review { "true" } else { "false" },
                )?;
                crate::task_engine::workflow_policy::sync_stored_policy_default_mode(&tx, review)?;
            }
            if let Some(raw) = &dsl {
                crate::db::system::set_system_config(&tx, "workflow_dsl", raw)?;
            }
            crate::db::system::set_system_config(&tx, "worker_poll_seconds", &poll.to_string())?;
            let data = worker_config_snapshot(&tx, poll)?;
            tx.commit()?;
            Ok(data)
        })()
    };
    let data = match saved {
        Ok(data) => data,
        Err(error) => {
            drop(guard);
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "WORKER_CONFIG_SAVE_FAILED",
                &super::errors::err_public(&error),
            )
            .await;
        }
    };
    guard.worker_loop_poll_seconds = poll;
    guard.last_error.clear();
    guard.last_event = "worker.loop.config_updated".to_string();
    guard.updated_at = unix_ts();
    drop(guard);
    let payload = json!({ "success": true, "data": data });
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
    log_file: &str,
    runtime_control: &WebUiRuntimeControl,
) -> anyhow::Result<()> {
    let _lifecycle = runtime_control.worker_lifecycle.lock().await;
    stop_worker_loop_inner(runtime_control, Duration::from_secs(2)).await;
    {
        let mut guard = state.lock().await;
        // A tick may finish naturally during the grace period. Only a forced
        // abort needs this fallback event; never duplicate its natural tail.
        let loop_needs_stopped_log = guard.worker_loop_running
            && guard.last_event != "worker.loop.stopped"
            && guard.last_event != "worker.loop.auth_failed";
        if loop_needs_stopped_log {
            guard.last_event = "worker.loop.stopped".to_string();
        }
        guard.worker_loop_running = false;
        guard.worker_status = "idle".to_string();
        guard.updated_at = unix_ts();
        if loop_needs_stopped_log {
            let _ = log_event(
                log_file,
                "info",
                "worker.loop.stopped",
                json!({ "via": "stop_endpoint" }),
            );
            let _ = flush_log();
        }
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

pub(crate) async fn stop_worker_loop(runtime_control: &WebUiRuntimeControl, grace: Duration) {
    let _lifecycle = runtime_control.worker_lifecycle.lock().await;
    stop_worker_loop_inner(runtime_control, grace).await;
}

async fn stop_worker_loop_inner(runtime_control: &WebUiRuntimeControl, grace: Duration) {
    // Never hold worker_handle while awaiting its natural tail: the tail takes
    // that mutex to clear its own handle. Start cannot publish a replacement yet.
    runtime_control
        .worker_running
        .store(false, Ordering::SeqCst);
    runtime_control.worker_stop.notify_waiters();
    let handle_opt = {
        let mut handle_guard = runtime_control.worker_handle.lock().await;
        handle_guard.take()
    };
    if let Some(mut handle) = handle_opt {
        if tokio::time::timeout(grace, &mut handle).await.is_err() {
            handle.abort();
            let _ = handle.await;
        }
    }
}
