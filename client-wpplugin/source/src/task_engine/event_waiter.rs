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
#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-event-waiter-rs
    // oracle: L1
    // 批 Q (事件驱动): waiter 契约——响应解析、唤醒合流、404 降级回退、
    // URL 装配（token/route_secret 解析同 worker 主环）。
    use super::*;

    #[test]
    fn wait_response_parses_ready_and_timeout_shapes() {
        let ready: EventsWaitResponse =
            serde_json::from_str(r#"{"success":true,"data":{"events_ready":true,"waited_ms":412}}"#)
                .unwrap();
        assert!(ready.success && ready.data.events_ready && ready.data.waited_ms == 412);
        let timeout: EventsWaitResponse =
            serde_json::from_str(r#"{"success":true,"data":{"events_ready":false,"waited_ms":25000}}"#)
                .unwrap();
        assert!(timeout.success && !timeout.data.events_ready);
        // Server shape drift (missing data) must not panic the waiter.
        let bare: EventsWaitResponse = serde_json::from_str(r#"{"success":true}"#).unwrap();
        assert!(!bare.data.events_ready);
    }

    #[tokio::test]
    async fn wait_loop_wakes_once_and_coalesces_while_draining() {
        // Plain-HTTP test server: disable the http transport-encryption
        // requirement the same way the discoverer tests do.
        let _transport_guard =
            crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
        // A server that reports events_ready=true on the first call and
        // holds later calls (never answers within the test window): the
        // loop must wake the notify, debounce, then block on the held
        // request — and repeated notify permits must coalesce to one.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = r#"{"success":true,"data":{"events_ready":true,"waited_ms":5}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
                body.len(),
                body
            );
            use tokio::io::AsyncWriteExt;
            let _ = stream.write_all(resp.as_bytes()).await;
            // Hold the connection: the next request on this stream never
            // gets a response inside the test window.
            let _ = tokio::time::sleep(Duration::from_secs(15)).await;
        });
        let cfg = EventWaitConfig {
            enabled: true,
            wait_seconds: 1,
            debounce_secs: 1,
            error_backoff_secs: 1,
            unsupported_retry_secs: 1,
        };
        let client = Arc::new(
            Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
        );
        let wake = Arc::new(Notify::new());
        let shutdown = CancellationToken::new();
        let url = format!("http://{addr}/wp-json/wptsall/v2/secret/client/events/wait?wait_seconds=1");
        let log_file = Arc::new(String::from("/tmp/wptsall-event-waiter-test.log"));
        let handle = tokio::spawn(wait_domain_loop_on_url(
            Arc::clone(&client),
            cfg,
            url,
            Arc::clone(&wake),
            shutdown.clone(),
        ));
        // First wake arrives within the debounce window.
        tokio::time::timeout(Duration::from_secs(4), wake.notified())
            .await
            .expect("first wake must fire");
        // Coalescing: two notifies while nobody waits store at most one
        // permit, so exactly one more notified() resolves without waiting.
        wake.notify_one();
        wake.notify_one();
        let second = tokio::time::timeout(Duration::from_millis(200), wake.notified()).await;
        assert!(second.is_ok(), "stored permit must resolve immediately");
        let third = tokio::time::timeout(Duration::from_millis(200), wake.notified()).await;
        assert!(third.is_err(), "permits must coalesce to one");
        shutdown.cancel();
        handle.abort();
        server.abort();
    }

    /// Test seam: drive the domain loop body against a fixed URL instead of
    /// resolving bindings (binding resolution is covered by the URL
    /// assembly test).
    async fn wait_domain_loop_on_url(
        client: Arc<Client>,
        cfg: EventWaitConfig,
        url: String,
        wake: Arc<Notify>,
        shutdown: CancellationToken,
    ) {
        let token = "test-token".to_string();
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            match wp_get_json_with_transport_and_secret::<EventsWaitResponse>(
                &client, &url, &token, "w-test", "d-test", None,
            )
            .await
            {
                Ok(resp) if resp.success && resp.data.events_ready => {
                    wake.notify_one();
                    if !sleep_or_shutdown(cfg.debounce_secs, &shutdown).await {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    if !sleep_or_shutdown(cfg.error_backoff_secs, &shutdown).await {
                        return;
                    }
                }
            }
        }
    }

    #[test]
    fn wait_url_assembles_with_secret_and_window() {
        let doc: DomainTokenBindingsDoc = serde_json::from_str(
            r#"{"version":3,"domains":{"http://127.0.0.1:9181":{"wp_client_token":"tok1","route_secret":"sec1"}}}"#,
        )
        .unwrap();
        let domains = domain_token_binding_local_sites(&doc);
        assert_eq!(domains.len(), 1, "binding map must resolve one local site");
        let api_base_url = &domains[0].api_base_url;
        let Some((url, token, secret)) = wait_url_for_domain(api_base_url, &doc, 25) else {
            panic!("binding must resolve");
        };
        assert!(
            url.ends_with("/wp-json/wptsall/v2/sec1/client/events/wait?wait_seconds=25"),
            "url: {url}"
        );
        assert_eq!(token, "tok1");
        assert_eq!(secret, "sec1");
    }
}