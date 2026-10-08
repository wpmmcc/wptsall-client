//! 批 Q (事件驱动 wake): outbox 事件长轮询唤醒器。
//!
//! M-3 裁定（批 M 冻结，本批解冻立项）：原 20s 轮询节奏（WPTSALL_POLL_SECONDS）
//! 下新 outbox 事件平均等待半 个 poll 窗口；事件驱动需 WP 侧推送通知端点
//! （新产品面）。WP 侧现已提供 `GET /{secret}/client/events/wait` 长轮询门：
//! 服务端以 0.5s 只读 peek 钉住 outbox 可领取性，就绪即刻返回。
//!
//! 本模块每绑定站点起一个并发等待协程（每站点至多占用一个 PHP worker 的
//! 挂起请求），就绪即 `Notify::notify_one()` 唤醒 worker 主环提前结束本轮
//! sleep。notify 的 permit 语义天然合流：主环忙于 drain 时多次唤醒至多存
//! 一枚 permit，drain 结束后立刻补一轮，不会惊群。
//!
//! 语义边界（与既有机制的关系）：
//! - 等待协程**只 peek 不 claim**——领取/租约/死租约回收/available_at 退避
//!   全部留在 content-changes 的 claim_outbox()，本端点对其零影响；
//! - 20s 常规轮询保持为兜底：等待协程挂掉/端点 404（旧版插件）/网络错误
//!   时回退为纯轮询节奏，行为不劣于批 Q 之前；
//! - run-once（one-shot）路径不启用等待协程。

use crate::auth::wp_get_json_with_transport_and_secret;
use crate::bindings::{
    build_wp_base_url, domain_token_binding_local_sites, load_domain_token_bindings,
    normalize_domain_base, resolve_route_secret_for_domain, resolve_wp_client_token_for_domain,
};
use crate::config::env_bool;
use crate::logging::log_event;
use crate::types::DomainTokenBindingsDoc;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// Waiter tuning. Defaults keep one held PHP request per site at ≤30s and
/// coalesce wakes so a drain always gets a full debounce window.
#[derive(Debug, Clone)]
pub(crate) struct EventWaitConfig {
    pub enabled: bool,
    /// Server hold window requested via `wait_seconds` (clamped 1..=30 —
    /// the WP endpoint rejects nothing above 30 by server-side clamp, and
    /// staying under it keeps the per-request timeout modest).
    pub wait_seconds: u64,
    /// After a wake, hold the re-arm so the worker's drain can claim before
    /// the waiter re-arms its long-poll (avoids wake/report loops on a row
    /// the drain cannot yet lease, e.g. an `available_at` backoff row the
    /// peek still sees as claimable once its backoff expires).
    pub debounce_secs: u64,
    /// Backoff after transport errors (network/5xx/deserialize).
    pub error_backoff_secs: u64,
    /// Re-probe cadence after a 404 (plugin predates the endpoint).
    pub unsupported_retry_secs: u64,
}

impl EventWaitConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            enabled: env_bool("WPTSALL_EVENT_WAIT_ENABLED", true),
            wait_seconds: crate::config::env_u64("WPTSALL_EVENT_WAIT_SECONDS", 25)
                .clamp(1, 30),
            debounce_secs: crate::config::env_u64("WPTSALL_EVENT_WAIT_DEBOUNCE_SECONDS", 2)
                .max(1),
            error_backoff_secs: crate::config::env_u64(
                "WPTSALL_EVENT_WAIT_ERROR_BACKOFF_SECONDS",
                30,
            )
            .max(1),
            unsupported_retry_secs: crate::config::env_u64(
                "WPTSALL_EVENT_WAIT_UNSUPPORTED_RETRY_SECONDS",
                300,
            )
            .max(1),
        }
    }
}

#[derive(Debug, Deserialize)]
struct EventsWaitResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    data: EventsWaitData,
}

#[derive(Debug, Deserialize, Default)]
struct EventsWaitData {
    #[serde(default)]
    events_ready: bool,
    #[serde(default)]
    waited_ms: u64,
}

/// Resolve the long-poll URL for one bound site. `None` when the binding
/// lacks a token or route secret (the worker loop skips those domains too).
fn wait_url_for_domain(
    api_base_url: &str,
    bindings: &DomainTokenBindingsDoc,
    wait_seconds: u64,
) -> Option<(String, String, String)> {
    let token = resolve_wp_client_token_for_domain(api_base_url, bindings, "")?;
    let secret = resolve_route_secret_for_domain(api_base_url, bindings)
        .or_else(|| {
            domain_token_binding_local_sites(bindings)
                .into_iter()
                .find(|d| d.api_base_url == api_base_url)
                .and_then(|d| d.route_secret)
        })?;
    let wp_base = build_wp_base_url(&normalize_domain_base(api_base_url), &secret)?;
    let url = format!("{wp_base}/events/wait?wait_seconds={wait_seconds}");
    Some((url, token, secret))
}

async fn sleep_or_shutdown(secs: u64, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(secs)) => true,
        _ = shutdown.cancelled() => false,
    }
}

/// One domain's wait loop: long-poll → wake → debounce → re-arm.
async fn wait_domain_loop(
    client: Arc<Client>,
    cfg: EventWaitConfig,
    worker_id: String,
    device_id: String,
    log_file: Arc<String>,
    api_base_url: String,
    bindings: DomainTokenBindingsDoc,
    wake: Arc<Notify>,
    shutdown: CancellationToken,
) {
    let Some((url, token, secret)) = wait_url_for_domain(&api_base_url, &bindings, cfg.wait_seconds)
    else {
        return;
    };
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        match wp_get_json_with_transport_and_secret::<EventsWaitResponse>(
            &client,
            &url,
            &token,
            &worker_id,
            &device_id,
            Some(&secret),
        )
        .await
        {
            Ok(resp) if resp.success && resp.data.events_ready => {
                let _ = log_event(
                    &log_file,
                    "info",
                    "worker.event_wake",
                    json!({
                        "api_base_url": api_base_url,
                        "waited_ms": resp.data.waited_ms,
                    }),
                );
                wake.notify_one();
                if !sleep_or_shutdown(cfg.debounce_secs, &shutdown).await {
                    return;
                }
            }
            // Clean timeout (events_ready=false) — re-arm immediately.
            Ok(_) => {}
            Err(err) => {
                let msg = format!("{err:#}");
                if msg.contains("status=404") {
                    // Older plugin without the endpoint: stop holding a
                    // request and re-probe later; plain polling covers the
                    // site in the meantime.
                    let _ = log_event(
                        &log_file,
                        "info",
                        "worker.event_wait_unsupported",
                        json!({
                            "api_base_url": api_base_url,
                            "retry_secs": cfg.unsupported_retry_secs,
                        }),
                    );
                    if !sleep_or_shutdown(cfg.unsupported_retry_secs, &shutdown).await {
                        return;
                    }
                } else {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "worker.event_wait_error",
                        json!({
                            "api_base_url": api_base_url,
                            "error": msg,
                        }),
                    );
                    if !sleep_or_shutdown(cfg.error_backoff_secs, &shutdown).await {
                        return;
                    }
                }
            }
        }
    }
}

/// Supervisor: reconciles the bound-domain set every few seconds, keeping
/// exactly one wait loop per currently-bound site. Removed bindings let
/// their loop run out on the next tick (the long-poll request is bounded
/// by wait_seconds + timeout anyway).
pub(crate) async fn run_event_waiter(
    cfg: EventWaitConfig,
    worker_id: String,
    device_id: String,
    log_file: String,
    wake: Arc<Notify>,
    shutdown: CancellationToken,
) {
    let client = match crate::auth::wp_http_client_builder()
        .timeout(Duration::from_secs(cfg.wait_seconds + 10))
        .build()
    {
        Ok(client) => Arc::new(client),
        Err(err) => {
            let _ = log_event(
                &log_file,
                "warning",
                "worker.event_wait_disabled",
                json!({ "reason": err.to_string() }),
            );
            return;
        }
    };
    let log_file = Arc::new(log_file);
    let _ = log_event(
        &log_file,
        "info",
        "worker.event_wait_started",
        json!({
            "wait_seconds": cfg.wait_seconds,
            "debounce_secs": cfg.debounce_secs,
        }),
    );
    let mut handles: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let bindings_path = crate::config::domain_token_bindings_file();
        let bindings = match load_domain_token_bindings(&bindings_path) {
            Ok(doc) => doc,
            Err(err) => {
                let _ = log_event(
                    &log_file,
                    "warning",
                    "worker.event_wait_bindings_error",
                    json!({
                        "path": bindings_path,
                        "error": err.to_string(),
                    }),
                );
                if !sleep_or_shutdown(cfg.error_backoff_secs, &shutdown).await {
                    break;
                }
                continue;
            }
        };
        let domains: Vec<String> = domain_token_binding_local_sites(&bindings)
            .into_iter()
            .map(|d| d.api_base_url)
            .collect();
        // Spawn wait loops for newly bound domains.
        for api_base_url in &domains {
            if !handles.contains_key(api_base_url) {
                let handle = tokio::spawn(wait_domain_loop(
                    Arc::clone(&client),
                    cfg.clone(),
                    worker_id.clone(),
                    device_id.clone(),
                    Arc::clone(&log_file),
                    api_base_url.clone(),
                    bindings.clone(),
                    Arc::clone(&wake),
                    shutdown.clone(),
                ));
                handles.insert(api_base_url.clone(), handle);
            }
        }
        // Reap finished loops and drop loops for unbound domains.
        let current: std::collections::HashSet<&String> = domains.iter().collect();
        handles.retain(|domain, handle| {
            if !current.contains(domain) || handle.is_finished() {
                handle.abort();
                false
            } else {
                true
            }
        });
        if !sleep_or_shutdown(5, &shutdown).await {
            break;
        }
    }
    for (_, handle) in handles.drain() {
        handle.abort();
    }
    let _ = log_event(&log_file, "info", "worker.event_wait_stopped", json!({}));
}
