pub(crate) mod routes;
pub(crate) mod static_html;
pub mod test_support;

use anyhow::{anyhow, Context};
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::auth::{is_auth_error_message, request_json_encrypted};
use crate::bindings::{
    build_wp_base_url, has_any_domain_token_bindings, normalize_api_base_url_key,
    normalize_domain_base, normalize_rule_component_bindings_doc, resolve_local_dev_binding,
    resolve_route_secret_for_domain, resolve_wp_client_token_for_domain,
};
use crate::component_rt::loader::{
    collect_configured_runtime_component_ids, load_component_runtimes,
};
use crate::config::*;
use crate::logging::{init_log_file, log_event, unix_ts};
use crate::persistence::PendingCallbackStore;
use crate::task_engine::discoverer::discover_and_translate;
use crate::types::*;

const CLIENT_MODE_HEADER: &str = "X-WPTSALL-Client-Mode";
const CLIENT_MODE_DESKTOP: &str = "desktop-client";
const LOCAL_DEV_LICENSE_STATUS: &str = "local_dev";
const DEFAULT_WEB_UI_BIND_ADDR: &str = "127.0.0.1:8977";
const DEFAULT_WEB_UI_PORT: u16 = 8977;

fn is_fatal_control_plane_error(err_text: &str) -> bool {
    is_auth_error_message(err_text) || err_text.contains("RATE_LIMITED")
}

/// IP-based access control for the Web UI TCP listener.
/// Default: only loopback (127.0.0.1 / ::1) allowed.
/// When external_access is enabled, additional IPs from the whitelist are allowed.
#[derive(Clone)]
pub(crate) struct AccessControl {
    inner: Arc<RwLock<AccessControlState>>,
}

struct AccessControlState {
    external_access: bool,
    allowed_ips: HashSet<IpAddr>,
}

impl AccessControl {
    pub(crate) fn new(external_access: bool, extra_ips: &[IpAddr]) -> Self {
        let mut allowed = HashSet::new();
        allowed.insert(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        allowed.insert(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
        for ip in extra_ips {
            allowed.insert(*ip);
        }
        Self {
            inner: Arc::new(RwLock::new(AccessControlState {
                external_access,
                allowed_ips: allowed,
            })),
        }
    }

    pub(crate) async fn is_allowed(&self, addr: IpAddr) -> bool {
        if addr.is_loopback() {
            return true;
        }
        let state = self.inner.read().await;
        state.allowed_ips.contains(&addr)
    }

    pub(crate) async fn is_external(&self) -> bool {
        self.inner.read().await.external_access
    }

    pub(crate) async fn get_settings(&self) -> (bool, Vec<String>) {
        let state = self.inner.read().await;
        let ips: Vec<String> = state
            .allowed_ips
            .iter()
            .filter(|ip| !ip.is_loopback())
            .map(|ip| ip.to_string())
            .collect();
        (state.external_access, ips)
    }

    pub(crate) async fn update(&self, external_access: bool, extra_ips: &[IpAddr]) {
        let mut state = self.inner.write().await;
        state.external_access = external_access;
        state.allowed_ips.clear();
        state
            .allowed_ips
            .insert(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        state
            .allowed_ips
            .insert(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
        for ip in extra_ips {
            state.allowed_ips.insert(*ip);
        }
    }
}

fn parse_web_ui_port_override() -> Option<u16> {
    std::env::var("WPTSALL_WEB_UI_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
}

fn parse_port_from_bind_addr(bind_addr: &str) -> Option<u16> {
    bind_addr.rsplit(':').next()?.parse::<u16>().ok()
}

fn replace_bind_addr_port(bind_addr: &str, port: u16) -> String {
    if let Some((host, _)) = bind_addr.rsplit_once(':') {
        format!("{}:{}", host, port)
    } else {
        format!("127.0.0.1:{}", port)
    }
}

pub(crate) fn web_ui_effective_bind_addr() -> String {
    let bind_addr = env_or("WPTSALL_WEB_UI_BIND", DEFAULT_WEB_UI_BIND_ADDR);
    match parse_web_ui_port_override() {
        Some(port) => replace_bind_addr_port(&bind_addr, port),
        None => bind_addr,
    }
}

pub(crate) fn web_ui_effective_port() -> u16 {
    parse_web_ui_port_override()
        .or_else(|| {
            parse_port_from_bind_addr(&env_or("WPTSALL_WEB_UI_BIND", DEFAULT_WEB_UI_BIND_ADDR))
        })
        .unwrap_or(DEFAULT_WEB_UI_PORT)
}

pub(crate) fn web_ui_loopback_origin() -> String {
    format!("http://127.0.0.1:{}", web_ui_effective_port())
}

/// Force SQLite-backed WebUI storage for the in-process WebUI/Desktop agent.
///
/// `main` only enters `run_web_ui` when `WPTSALL_WEB_UI` is already truthy, but
/// Desktop embeds the agent by calling `run_web_ui` directly without that env.
/// Loader/routes gate JSON vs SQLite on the same flag; without it UI writes
/// land in SQLite while the worker still reads JSON and installed components
/// appear "missing". Always pin the flag when the WebUI runtime starts.
pub fn ensure_web_ui_storage_mode() {
    // Called once at process startup of the WebUI/Desktop agent before
    // concurrent workers touch component storage. Overwrites a false/absent
    // value so embedded Desktop cannot drift into JSON mode.
    std::env::set_var("WPTSALL_WEB_UI", "1");
}

pub async fn run_web_ui(
    shutdown_token: CancellationToken,
    start_time: std::time::Instant,
) -> anyhow::Result<()> {
    ensure_web_ui_storage_mode();
    let bind_addr_env = web_ui_effective_bind_addr();
    let worker_loop_poll_seconds = env_u64("WPTSALL_POLL_SECONDS", 20).max(1);
    let log_file = env_or("WPTSALL_LOG_FILE", DEFAULT_LOG_FILE);
    let component_bindings_path = env_or(
        "WPTSALL_COMPONENT_BINDINGS_FILE",
        "./config/component-bindings.json",
    );
    let domain_token_bindings_path = env_or(
        "WPTSALL_DOMAIN_TOKEN_BINDINGS_FILE",
        DEFAULT_DOMAIN_TOKEN_BINDINGS_FILE,
    );
    let task_type_component_bindings_path = env_or(
        "WPTSALL_TASK_TYPE_COMPONENT_BINDINGS_FILE",
        DEFAULT_TASK_TYPE_COMPONENT_BINDINGS_FILE,
    );
    let rule_component_bindings_path = env_or(
        "WPTSALL_RULE_COMPONENT_BINDINGS_FILE",
        DEFAULT_RULE_COMPONENT_BINDINGS_FILE,
    );
    init_log_file(&log_file)?;

    // Open SQLite database and run one-time JSON migration
    let db_path = env_or("WPTSALL_DB_PATH", "./runtime/wptsall.db");
    let db_conn = crate::db::open_db(&db_path).context("Failed to open SQLite database")?;
    crate::db::migrate_from_json_if_needed(&db_conn).ok();

    // Load bindings from DB (populated by migration above if first run, or from previous DB writes)
    let component_bindings = crate::db::bindings::load_component_bindings_doc(&db_conn);
    let domain_token_bindings = crate::db::bindings::load_domain_token_bindings_doc(&db_conn);
    let task_type_component_bindings =
        crate::db::bindings::load_task_type_component_bindings_doc(&db_conn);
    let rule_component_bindings = crate::db::bindings::load_rule_component_bindings_doc(&db_conn);
    let normalized_rule_component_bindings =
        normalize_rule_component_bindings_doc(&rule_component_bindings);
    let bindings_changed = serde_json::to_string(&normalized_rule_component_bindings).ok()
        != serde_json::to_string(&rule_component_bindings).ok();
    if bindings_changed {
        let _ = crate::db::bindings::save_rule_component_bindings_doc(
            &db_conn,
            &normalized_rule_component_bindings,
        );
    }
    let rule_component_bindings = normalized_rule_component_bindings;

    // No implicit control-plane URL: local-first mode must remain fully local,
    // while the legacy server lane is enabled only by explicit configuration.
    let server_base = configured_server_base();
    // device_id: prefer non-empty env var, then load-or-create from SQLite DB (stable across restarts)
    let device_id = match std::env::var("WPTSALL_DEVICE_ID") {
        Ok(id) if !id.trim().is_empty() => id.trim().to_string(),
        _ => crate::db::system::load_or_create_device_id(&db_conn)
            .context("Failed to load or create device_id from DB")?,
    };

    // Load log settings from DB and apply to global atomics.
    // Release default: logging DISABLED — only an explicit DB value "true"
    // (set via the Settings page) or WPTSALL_LOG_ENABLED=1 turns it on.
    let log_enabled = crate::logging::resolve_log_enabled(
        crate::db::system::get_system_config(&db_conn, "log_enabled").as_deref(),
        std::env::var("WPTSALL_LOG_ENABLED").ok().as_deref(),
    );
    let log_min_level = crate::db::system::get_system_config(&db_conn, "log_min_level")
        .unwrap_or_else(|| "info".to_string());
    crate::logging::set_log_enabled(log_enabled);
    crate::logging::set_log_min_level(&log_min_level);

    // Load access control settings from DB
    let external_access = crate::db::system::get_system_config(&db_conn, "web_ui_external_access")
        .map(|v| v == "true")
        .unwrap_or(false);
    let allowed_ips_str =
        crate::db::system::get_system_config(&db_conn, "web_ui_allowed_ips").unwrap_or_default();
    let extra_ips: Vec<IpAddr> = allowed_ips_str
        .split(',')
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .collect();
    let access_control = AccessControl::new(external_access, &extra_ips);

    // Determine bind address: if external_access is enabled, bind to 0.0.0.0;
    // otherwise respect the env var (default 127.0.0.1).
    let bind_addr = if external_access {
        // Extract port from env-configured address, bind to all interfaces
        let port = bind_addr_env.rsplit(':').next().unwrap_or("8977");
        format!("0.0.0.0:{}", port)
    } else {
        bind_addr_env
    };

    // Wrap the DB connection in Arc<Mutex> for shared access across request handlers
    let db = std::sync::Arc::new(tokio::sync::Mutex::new(db_conn));

    // Create a shared HTTP client with connection pooling (reused across all worker runs).
    // Test-hosts may opt in to insecure TLS for private CA / self-signed environments.
    let allow_insecure_tls = env_bool("WPTSALL_ALLOW_INSECURE_TLS", false);
    let mut http_client_builder = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(90))
        .connect_timeout(Duration::from_secs(10));
    if allow_insecure_tls {
        eprintln!(
            "warning: WPTSALL_ALLOW_INSECURE_TLS=true, TLS certificate verification is disabled"
        );
        http_client_builder = http_client_builder.danger_accept_invalid_certs(true);
    }
    let http_client = http_client_builder.build()?;
    let use_server_control_plane = server_control_plane_enabled();

    let restored_session_token = if use_server_control_plane {
        if let Some(token) = crate::oauth::read_cached_token_db(&db_path) {
            if crate::oauth::validate_session_token(&http_client, &server_base, &device_id, &token)
                .await
            {
                Some(token)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    let initial_domains = if use_server_control_plane {
        Vec::new()
    } else {
        local_sites_from_domain_token_bindings(&domain_token_bindings)
    };

    let initial_state = WebUiState {
        server_base,
        device_id,
        session_token: restored_session_token,
        oauth_code_verifier: None,
        oauth_state: None,
        domains: initial_domains,
        components: Vec::new(),
        component_bindings_path,
        component_bindings,
        domain_token_bindings_path,
        domain_token_bindings,
        task_type_component_bindings_path,
        task_type_component_bindings,
        rule_component_bindings_path,
        rule_component_bindings,
        worker_loop_running: false,
        worker_status: "idle".to_string(),
        worker_loop_poll_seconds,
        worker_last_summary: json!({}),
        worker_recent_runs: Vec::new(),
        local_components_backfilled: 0,
        local_components_backfill_error: String::new(),
        last_error: String::new(),
        last_event: "webui.ready".to_string(),
        updated_at: unix_ts(),
        log_enabled,
        log_min_level,
        vendor_oauth_pending: std::collections::HashMap::new(),
        db,
        http_client,
        update_in_progress: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let shared_state = Arc::new(Mutex::new(initial_state));
    let runtime_control = WebUiRuntimeControl::new();

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("bind web ui failed: {}", bind_addr))?;
    eprintln!(
        "{}",
        crate::i18n::t("cli.webui_listening", &crate::i18n::detect_cli_locale())
            .replacen("{}", &bind_addr, 1)
    );

    // Auto-start worker loop if configured and there is a local site registry
    {
        let auto_start = {
            let guard = shared_state.lock().await;
            let conn = guard.db.lock().await;
            crate::db::system::get_system_config(&conn, "auto_start_worker")
                .map(|v| v == "true")
                .unwrap_or(false)
        };
        if auto_start {
            let can_start = {
                let guard = shared_state.lock().await;
                if use_server_control_plane {
                    guard.session_token.is_some()
                } else {
                    !guard.domains.is_empty()
                }
            };
            if can_start {
                let auto_state = Arc::clone(&shared_state);
                let auto_runtime = runtime_control.clone();
                let auto_log_file = log_file.clone();
                routes::spawn_worker_loop(auto_state, auto_runtime, auto_log_file, "auto").await;
            }
        }
    }
    let started_at_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let _ = log_event(
        &log_file,
        "info",
        "webui.started",
        json!({ "bind": bind_addr }),
    );

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (socket, addr) = result?;

                // IP-based access control: reject connections from non-whitelisted IPs
                if !access_control.is_allowed(addr.ip()).await {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "webui.connection_rejected",
                        json!({ "peer_ip": addr.ip().to_string() }),
                    );
                    drop(socket);
                    continue;
                }

                let state = Arc::clone(&shared_state);
                let runtime_control = runtime_control.clone();
                let log_file = log_file.clone();
                let ac = access_control.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        routes::handle_web_ui_connection(
                            socket, state, runtime_control, &log_file,
                            started_at_epoch, start_time, ac,
                        ).await
                    {
                        let _ = log_event(
                            &log_file,
                            "warning",
                            "webui.connection_error",
                            json!({ "error": format!("{:#}", err) }),
                        );
                    }
                });
            }
            _ = shutdown_token.cancelled() => {
                let _ = log_event(&log_file, "info", "webui.shutdown_requested", json!({}));
                // Stop worker loop if running
                {
                    let mut guard = shared_state.lock().await;
                    if guard.worker_loop_running {
                        guard.worker_loop_running = false;
                        guard.worker_status = "stopping".to_string();
                    }
                }
                // Give in-flight connections a brief grace period
                tokio::time::sleep(Duration::from_secs(2)).await;
                break;
            }
        }
    }

    let _ = log_event(&log_file, "info", "webui.shutdown_complete", json!({}));
    crate::logging::flush_log();
    Ok(())
}

pub(crate) async fn web_ui_run_worker_once(
    state: &Arc<Mutex<WebUiState>>,
    log_file: &str,
) -> anyhow::Result<Value> {
    web_ui_run_worker_once_with_limits(state, log_file, None).await
}

pub(crate) fn local_sites_from_domain_token_bindings(
    doc: &DomainTokenBindingsDoc,
) -> Vec<DomainStatusItem> {
    crate::bindings::domain_token_binding_local_sites(doc)
}

pub(crate) async fn web_ui_run_worker_once_with_limits(
    state: &Arc<Mutex<WebUiState>>,
    log_file: &str,
    max_items_per_run: Option<usize>,
) -> anyhow::Result<Value> {
    let _max_items_guard =
        crate::task_engine::discoverer::scoped_max_items_per_run_override(max_items_per_run);
    let use_server_control_plane = server_control_plane_enabled();
    let (
        server_base,
        device_id,
        existing_session_token,
        domain_token_bindings,
        task_type_component_bindings,
        db_arc,
        client,
    ) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.device_id.clone(),
            guard.session_token.clone(),
            guard.domain_token_bindings.clone(),
            guard.task_type_component_bindings.clone(),
            std::sync::Arc::clone(&guard.db),
            guard.http_client.clone(),
        )
    };

    let wp_client_token_fallback = if use_server_control_plane {
        env_or("WPTSALL_WP_CLIENT_TOKEN", "")
    } else {
        String::new()
    };
    if use_server_control_plane
        && wp_client_token_fallback.trim().is_empty()
        && !has_any_domain_token_bindings(&domain_token_bindings)
    {
        return Err(anyhow!(
            "MISSING_WP_CLIENT_TOKEN: set WPTSALL_WP_CLIENT_TOKEN or configure per-domain token bindings before running worker"
        ));
    }

    let component_runtime_enabled = env_bool("WPTSALL_COMPONENT_RUNTIME", true);
    let component_id_override = env_or("WPTSALL_COMPONENT_ID", "");
    let component_prefer_ids = parse_csv_env("WPTSALL_COMPONENT_PREFER_IDS");
    let component_bindings_path = env_or(
        "WPTSALL_COMPONENT_BINDINGS_FILE",
        "./config/component-bindings.json",
    );
    let mut worker_config = crate::worker::build_worker_config(&device_id);
    // Override review_mode from DB (Web UI toggle takes priority over env var)
    {
        let conn = db_arc.lock().await;
        if let Some(val) = crate::db::system::get_system_config(&conn, "review_mode") {
            worker_config.review_mode = val == "true";
        }
    }

    if !component_runtime_enabled && !worker_config.component_fallback_enabled {
        return Err(anyhow!(
            "INVALID_WORKER_CONFIG: WPTSALL_COMPONENT_RUNTIME=0 requires WPTSALL_COMPONENT_FALLBACK=1"
        ));
    }

    let (session_token, domains) = if use_server_control_plane {
        let session_token = existing_session_token
            .ok_or_else(|| anyhow!("NOT_LOGGED_IN: please login via OAuth first (click Login)"))?;
        let domains = fetch_domains_for_session(&client, &server_base, &session_token).await?;
        (Some(session_token), domains)
    } else {
        (
            None,
            local_sites_from_domain_token_bindings(&domain_token_bindings),
        )
    };

    let result = async move {
        // Fast path: no sites configured -> return success immediately.
        // Local-first mode treats the registry as the source of truth.
        if domains.is_empty() {
            let _ = log_event(
                log_file,
                "info",
                "worker.run_once.empty_domains",
                json!({
                    "domains_count": 0,
                    "mode": if use_server_control_plane { "server" } else { "local" }
                }),
            );
            let summary = json!({
                "domains_processed": 0,
                "tasks_processed": 0,
                "tasks_succeeded": 0,
                "tasks_failed": 0,
                "total_items": 0,
                "summary": {
                    "domain_count": 0,
                    "domains_total": 0,
                    "skipped_missing_token": 0,
                    "missing_token_domains": [],
                    "skipped_missing_route_secret": 0,
                    "missing_route_secret_domains": [],
                    "domains": []
                }
            });
            return Ok::<(Option<String>, Vec<DomainStatusItem>, Value), anyhow::Error>((
                session_token,
                domains,
                summary,
            ));
        }

        let db_path = env_or("WPTSALL_DB_PATH", "./runtime/wptsall.db");
        let pending_callback_store = Arc::new(Mutex::new(PendingCallbackStore::open(&db_path)?));

        let signing_key_from_db = if use_server_control_plane {
            // Prefer the cached signing key from DB. A refresh failure should only be fatal
            // when we have no trusted key material locally; otherwise run-once can continue
            // with the cached key and avoid being interrupted by transient server rate limits.
            let cached_signing_key_from_db = {
                let conn = db_arc.lock().await;
                crate::db::system::get_signing_key(&conn)
                    .filter(|pem| pem.trim().starts_with("-----BEGIN PUBLIC KEY-----"))
            };
            {
                let session_token_str = session_token.as_deref().unwrap_or("");
                let key_url = format!("{}/api/v1/client/signing-public-key", server_base);
                match crate::auth::request_json_encrypted::<
                        crate::types::ApiResponse<serde_json::Value>,
                    >(
                        client
                            .get(&key_url)
                            .header("X-Client-Session", session_token_str),
                        "signing-public-key",
                        session_token_str,
                    )
                    .await
                    {
                        Ok(resp) if resp.success => {
                            if let Some(pem) = resp.data.get("public_key_pem").and_then(|v| v.as_str())
                            {
                                if pem.contains("BEGIN PUBLIC KEY") {
                                    let key_id = resp
                                        .data
                                        .get("key_id")
                                        .and_then(|v| v.as_str())
                                        .map(str::trim)
                                        .filter(|v| !v.is_empty());
                                    {
                                        let conn = db_arc.lock().await;
                                        let _ = crate::db::system::set_signing_key_material(
                                            &conn, pem, key_id,
                                        );
                                    }
                                    let _ = log_event(
                                        log_file,
                                        "info",
                                        "component.signing_key_fetched",
                                        json!({ "key_id": key_id.unwrap_or("unknown") }),
                                    );
                                }
                            }
                        }
                        Ok(_) => {
                            let _ = log_event(
                                log_file,
                                "warning",
                                "component.signing_key_fetch_failed",
                                json!({ "note": "server returned success=false" }),
                            );
                        }
                        Err(err) => {
                            let err_text = format!("{:#}", err);
                            let _ = log_event(
                                log_file,
                                "warning",
                                "component.signing_key_fetch_failed",
                                json!({ "error": err_text }),
                            );
                            if cached_signing_key_from_db.is_none()
                                && is_fatal_control_plane_error(&format!("{:#}", err))
                            {
                                return Err(err);
                            }
                        }
                    }
            }
            let conn = db_arc.lock().await;
            crate::db::system::get_signing_key(&conn)
                .filter(|pem| pem.trim().starts_with("-----BEGIN PUBLIC KEY-----"))
        } else {
            None
        };

        let component_registry: Option<Arc<ComponentRuntimeRegistry>> = if component_runtime_enabled
        {
            let (mut component_bindings, task_type_component_bindings, rule_component_bindings) = {
                let guard = state.lock().await;
                (
                    guard.component_bindings.clone(),
                    guard.task_type_component_bindings.clone(),
                    guard.rule_component_bindings.clone(),
                )
            };
            let local_components_doc = crate::db::components::load_runtime_local_components_doc();
            let target_component_ids = collect_configured_runtime_component_ids(
                &local_components_doc,
                Some(&task_type_component_bindings),
                Some(&rule_component_bindings),
                Some(&component_bindings),
            );
            let session_token_str = session_token.as_deref().unwrap_or("");
            match load_component_runtimes(
                &client,
                &server_base,
                session_token_str,
                log_file,
                &mut component_bindings,
                &component_bindings_path,
                Some(&target_component_ids),
                signing_key_from_db.as_deref(),
            )
            .await
            {
                Ok(registry) => {
                    let mut guard = state.lock().await;
                    guard.component_bindings = component_bindings;
                    Some(Arc::new(registry))
                }
                Err(err) => {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "component.runtime_disabled",
                        json!({ "reason": format!("{:#}", err) }),
                    );
                    None
                }
            }
        } else {
            None
        };

        let mut missing_token_domains: Vec<String> = Vec::new();
        let mut missing_route_secret_domains: Vec<String> = Vec::new();
        let task_type_bindings_arc = Arc::new(task_type_component_bindings.clone());
        let rule_component_bindings_arc = {
            let guard = state.lock().await;
            Arc::new(guard.rule_component_bindings.clone())
        };

        // Read governor config from DB or use env defaults
        let (dc, gtc, gcc) = {
            let defaults = crate::resource_governor::ResourceGovernor::from_env();
            let conn = db_arc.lock().await;
            let dc = crate::db::system::get_system_config(&conn, "domain_concurrency")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(defaults.domain_concurrency);
            let gtc = crate::db::system::get_system_config(&conn, "global_translation_concurrency")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(defaults.global_translation_concurrency);
            let gcc = crate::db::system::get_system_config(&conn, "global_callback_concurrency")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(defaults.global_callback_concurrency);
            (dc, gtc, gcc)
        };
        let governor = Arc::new(crate::resource_governor::ResourceGovernor::with_limits(
            dc, gtc, gcc,
        ));

        // Prepare domain tasks
        struct WebUiDomainTask {
            api_base_url: String,
            wp_client_token: String,
            route_secret: Option<String>,
            wp_base: String,
            domain_base: String,
            is_local_dev: bool,
            relation_limit: Option<usize>,
        }
        let mut domain_tasks: Vec<WebUiDomainTask> = Vec::new();
        let occupied_domain_bases: HashSet<String> = domains
            .iter()
            .filter(|d| d.site_status != LOCAL_DEV_LICENSE_STATUS)
            .map(|d| normalize_domain_base(&d.api_base_url))
            .filter(|v| !v.is_empty())
            .collect();
        for domain in &domains {
            let is_local_dev = domain.site_status == LOCAL_DEV_LICENSE_STATUS;
            // Free users still proceed — do NOT skip non-active domains.
            let (domain_api_base, wp_client_token, domain_route_secret) = if is_local_dev {
                if let Some((local_base, token, secret)) =
                    resolve_local_dev_binding(&domain_token_bindings, &occupied_domain_bases)
                {
                    (local_base, token, secret)
                } else {
                    let _ = log_event(
                        log_file,
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
                    missing_token_domains.push(domain.api_base_url.clone());
                    let _ = log_event(
                        log_file,
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
                        log_file,
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
            domain_tasks.push(WebUiDomainTask {
                api_base_url: domain_api_base,
                wp_client_token,
                route_secret: domain_route_secret,
                wp_base,
                domain_base,
                is_local_dev,
                relation_limit,
            });
        }

        // Fast path: no runnable domain tasks (all skipped or empty).
        // Valid production optimization: if every domain is missing token or
        // route secret, there is no work to dispatch. Returns success with
        // zero stats instead of running the heavy worker engine path.
        if domain_tasks.is_empty() {
            let _ = log_event(
                log_file,
                "info",
                "worker.run_once.empty_domain_tasks",
                json!({
                    "domains_total": domains.len(),
                    "skipped_missing_token": missing_token_domains.len(),
                    "skipped_missing_route_secret": missing_route_secret_domains.len(),
                }),
            );
            let summary = json!({
                "domains_processed": 0,
                "tasks_processed": 0,
                "tasks_succeeded": 0,
                "tasks_failed": 0,
                "total_items": 0,
                "summary": {
                    "domain_count": 0,
                    "domains_total": domains.len(),
                    "skipped_missing_token": missing_token_domains.len(),
                    "missing_token_domains": missing_token_domains,
                    "skipped_missing_route_secret": missing_route_secret_domains.len(),
                    "missing_route_secret_domains": missing_route_secret_domains,
                    "domains": []
                }
            });
            return Ok::<(Option<String>, Vec<DomainStatusItem>, Value), anyhow::Error>((
                session_token,
                domains,
                summary,
            ));
        }

        // Spawn concurrent domain workers
        let mut domain_joins: tokio::task::JoinSet<anyhow::Result<Option<DomainRunReport>>> =
            tokio::task::JoinSet::new();

        for dt in domain_tasks {
            let client = client.clone();
            let server_base = server_base.clone();
            let session_token = session_token.clone();
            let log_file = log_file.to_string();
            let component_registry = component_registry.clone();
            let component_id_override = component_id_override.clone();
            let component_prefer_ids = component_prefer_ids.clone();
            let task_type_bindings = Arc::clone(&task_type_bindings_arc);
            let rule_bindings = Arc::clone(&rule_component_bindings_arc);
            let worker_config = worker_config.clone();
            let pending_callback_store = Arc::clone(&pending_callback_store);
            let db_arc = std::sync::Arc::clone(&db_arc);
            let governor = Arc::clone(&governor);

                let shared_state = Arc::clone(state);
                domain_joins.spawn(async move {
                let Some(_domain_permit) = governor.domain_sem.acquire().await.ok() else {
                    return Ok(None);
                };

                let mut run_wp_base = dt.wp_base.clone();
                let mut run_route_secret = dt.route_secret.clone();

                let mut result = discover_and_translate(
                    &client,
                    &run_wp_base,
                    &dt.wp_client_token,
                    &log_file,
                    component_registry.clone(),
                    None,
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
                    Some(db_arc.clone()),
                )
                .await;

                // On 404, refresh domains from server and retry once with updated route_secret.
                if use_server_control_plane
                    && !dt.is_local_dev
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
                    let session_token_str = session_token.as_deref().unwrap_or("");
                    if let Ok(hv) = session_token_str.parse() {
                        refresh_headers.insert("X-Client-Session", hv);
                    }
                    if let Ok(hv) = CLIENT_MODE_DESKTOP.parse() {
                        refresh_headers.insert(CLIENT_MODE_HEADER, hv);
                    }
                    let refreshed_raw =
                        request_json_encrypted::<Value>(
                            client.get(&refresh_url).headers(refresh_headers),
                            "refresh domains (404 recovery)",
                            session_token_str,
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
                                return Err(anyhow!(
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
                                    None,
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
                                Some(db_arc.clone()),
                            )
                            .await;

                            if result.is_ok() {
                                if let Some(ref new_s) = run_route_secret {
                                    let path = {
                                        let guard = shared_state.lock().await;
                                        guard.domain_token_bindings_path.clone()
                                    };
                                    let doc = {
                                        let conn = db_arc.lock().await;
                                        let mut doc = crate::db::bindings::load_domain_token_bindings_doc(&conn);
                                        if let Some(entry) = doc.domains.get_mut(&dt.domain_base) {
                                            entry.route_secret = new_s.clone();
                                        } else {
                                            let legacy_key = normalize_api_base_url_key(&dt.api_base_url);
                                            if let Some(entry) = doc.domains.get_mut(&legacy_key) {
                                                entry.route_secret = new_s.clone();
                                            } else {
                                                doc.domains.insert(
                                                    dt.domain_base.clone(),
                                                    DomainTokenBindingEntry {
                                                        wp_client_token: dt.wp_client_token.clone(),
                                                        route_secret: new_s.clone(),
                                                    },
                                                );
                                            }
                                        }
                                        let _ = crate::db::bindings::save_domain_token_bindings_doc(&conn, &doc);
                                        doc
                                    };
                                    let _ = crate::bindings::save_domain_token_bindings(&path, &doc);
                                    {
                                        let mut guard = shared_state.lock().await;
                                        guard.domain_token_bindings = doc.clone();
                                    }
                                    let _ = log_event(
                                        &log_file,
                                        "info",
                                        "domain.route_secret_persisted",
                                        json!({ "domain": dt.domain_base }),
                                    );
                                }
                            }
                        }
                    }
                }

                // Simplified WP token rotation retry: 1 attempt after 30s
                if result
                    .as_ref()
                    .err()
                    .map(|err| crate::auth::is_wp_token_rotation_error(&format!("{:#}", err)))
                    .unwrap_or(false)
                {
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "domain.wp_token_rotation_detected",
                        json!({ "api_base_url": dt.api_base_url }),
                    );
                    // WP token rotation retry delay (30s in production; 0 in test envs that
                    // never rotate the token, e.g. when the licensed WP plugin is absent).
                    let rotation_retry_secs = std::env::var("WPTSALL_WP_TOKEN_ROTATION_RETRY_SECS")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(30);
                    if rotation_retry_secs > 0 {
                        tokio::time::sleep(std::time::Duration::from_secs(rotation_retry_secs)).await;
                    } else {
                        let _ = log_event(
                            &log_file,
                            "info",
                            "worker.wp_token_rotation_skipped",
                            json!({ "api_base_url": dt.api_base_url, "reason": "retry_secs=0" }),
                        );
                    }
                    let refreshed_token = {
                        let guard = shared_state.lock().await;
                        let fallback_wp_client_token = crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "");
                        crate::bindings::resolve_wp_client_token_for_domain(
                            &dt.api_base_url,
                            &guard.domain_token_bindings,
                            &fallback_wp_client_token,
                        )
                        .unwrap_or_else(|| dt.wp_client_token.clone())
                    };
                    result = discover_and_translate(
                        &client,
                        &run_wp_base,
                        &refreshed_token,
                        &log_file,
                        component_registry,
                        None,
                        &component_id_override,
                        &component_prefer_ids,
                        Some(task_type_bindings),
                        Some(rule_bindings.as_ref()),
                        &worker_config,
                        run_route_secret.as_deref(),
                        &pending_callback_store,
                        Some(Arc::clone(&governor.global_translation_sem)),
                        Some(Arc::clone(&governor.global_callback_sem)),
                        dt.relation_limit,
                        Some(db_arc),
                    )
                    .await;
                }

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
                        if is_fatal_control_plane_error(&format!("{:#}", err)) {
                            Err(err)
                        } else {
                            Ok(None)
                        }
                    }
                }
            });
        }

        // Collect results
        let mut loop_reports: Vec<DomainRunReport> = Vec::new();
        let mut fatal_domain_error: Option<anyhow::Error> = None;
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
                            Some(anyhow!("worker domain task join failed: {}", err));
                    }
                }
            }
        }
        if let Some(err) = fatal_domain_error {
            return Err(err);
        }

        let total_items_this_run: usize = loop_reports.iter().map(|r| r.processed).sum();
        let summary = json!({
            "domains_processed": loop_reports.len(),
            "tasks_processed": total_items_this_run,
            "tasks_succeeded": loop_reports.iter().map(|r| r.completed).sum::<usize>(),
            "tasks_failed": loop_reports.iter().map(|r| r.failed).sum::<usize>(),
            "total_items": total_items_this_run,
            "summary": {
                "domain_count": loop_reports.len(),
                "domains_total": domains.len(),
                "skipped_missing_token": missing_token_domains.len(),
                "missing_token_domains": missing_token_domains,
                "skipped_missing_route_secret": missing_route_secret_domains.len(),
                "missing_route_secret_domains": missing_route_secret_domains,
                "domains": loop_reports
                    .iter()
                    .map(|r| json!({
                        "api_base_url": r.api_base_url,
                        "processed": r.processed,
                        "completed": r.completed,
                        "failed": r.failed,
                        "avg_elapsed_ms": r.avg_elapsed_ms
                    }))
                    .collect::<Vec<Value>>()
            }
        });

        Ok::<(Option<String>, Vec<DomainStatusItem>, Value), anyhow::Error>((
            session_token,
            domains,
            summary,
        ))
    }
    .await;

    match result {
        Ok((session_token, domains, summary)) => {
            let mut guard = state.lock().await;
            guard.session_token = session_token;
            guard.domains = domains;
            guard.worker_last_summary = summary.clone();
            guard.worker_recent_runs.push(WebUiWorkerRunRecord {
                ts: unix_ts(),
                status: "success".to_string(),
                summary: summary.clone(),
                error: String::new(),
            });
            if guard.worker_recent_runs.len() > MAX_WEB_UI_WORKER_RUN_RECORDS {
                let drop_count = guard
                    .worker_recent_runs
                    .len()
                    .saturating_sub(MAX_WEB_UI_WORKER_RUN_RECORDS);
                guard.worker_recent_runs.drain(0..drop_count);
            }
            guard.last_error.clear();
            guard.last_event = "worker.run_once.completed".to_string();
            guard.updated_at = unix_ts();
            Ok(summary)
        }
        Err(err) => {
            let err_text = format!("{:#}", err);
            {
                let mut guard = state.lock().await;
                guard.worker_recent_runs.push(WebUiWorkerRunRecord {
                    ts: unix_ts(),
                    status: "failed".to_string(),
                    summary: json!({}),
                    error: err_text.clone(),
                });
                if guard.worker_recent_runs.len() > MAX_WEB_UI_WORKER_RUN_RECORDS {
                    let drop_count = guard
                        .worker_recent_runs
                        .len()
                        .saturating_sub(MAX_WEB_UI_WORKER_RUN_RECORDS);
                    guard.worker_recent_runs.drain(0..drop_count);
                }
                guard.last_error = err_text.clone();
                guard.last_event = "worker.run_once.failed".to_string();
                guard.updated_at = unix_ts();
            }
            Err(err)
        }
    }
}

pub(crate) async fn fetch_domains_for_session(
    client: &Client,
    server_base: &str,
    session_token: &str,
) -> anyhow::Result<Vec<DomainStatusItem>> {
    let mut headers = HeaderMap::new();
    headers.insert("X-Client-Session", session_token.parse()?);
    headers.insert(CLIENT_MODE_HEADER, CLIENT_MODE_DESKTOP.parse()?);
    let domains_url = format!("{}/api/v1/client/domains", server_base);
    let domains_raw: Value = request_json_encrypted(
        client.get(domains_url).headers(headers),
        "client domains",
        session_token,
    )
    .await?;
    let domain_items = parse_domains_data_value(domains_raw)?.items;
    let mut items: Vec<DomainStatusItem> = domain_items
        .into_iter()
        .map(|item| {
            let route_secret = item.route_secret.and_then(|s| {
                let normalized = s.trim().to_string();
                if normalized.is_empty() {
                    None
                } else {
                    Some(normalized)
                }
            });
            DomainStatusItem {
                api_base_url: item.api_base_url,
                site_status: item.site_status,
                route_secret,
                max_relations: item.max_relations,
                plan_expires_at: item.plan_expires_at,
            }
        })
        .collect();
    items.sort_by(|a, b| a.api_base_url.cmp(&b.api_base_url));
    Ok(items)
}

pub(crate) async fn fetch_vendors_for_session(
    client: &Client,
    server_base: &str,
    session_token: &str,
) -> anyhow::Result<Vec<VendorItem>> {
    let mut headers = HeaderMap::new();
    headers.insert("X-Client-Session", session_token.parse()?);
    let list_url = format!("{}/api/v1/client/vendors", server_base);
    let list_raw: Value = request_json_encrypted(
        client.get(list_url).headers(headers),
        "client vendors",
        session_token,
    )
    .await?;
    let mut items = if let Ok(resp) =
        serde_json::from_value::<ApiResponse<VendorsPagedData>>(list_raw.clone())
    {
        resp.data.items
    } else if let Ok(data) = serde_json::from_value::<VendorsPagedData>(list_raw.clone()) {
        data.items
    } else {
        anyhow::bail!("client vendors: unsupported response shape ({})", list_raw);
    };
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(items)
}

/// Cached server signing public key from local DB (used for L1/L2 catalog verify).
pub(crate) async fn read_trusted_signing_key_pem_from_state(
    state: &Arc<Mutex<WebUiState>>,
) -> Option<String> {
    let db_arc = {
        let guard = state.lock().await;
        Arc::clone(&guard.db)
    };
    let conn = db_arc.lock().await;
    crate::db::system::get_signing_key(&conn)
        .filter(|pem| pem.trim().starts_with("-----BEGIN PUBLIC KEY-----"))
}

#[allow(dead_code)]
pub async fn fetch_wp_translation_providers_for_session(
    client: &Client,
    server_base: &str,
    session_token: &str,
) -> anyhow::Result<Vec<WpTranslationProviderItem>> {
    fetch_wp_translation_providers_for_session_with_signing_key(
        client,
        server_base,
        session_token,
        None,
    )
    .await
}

pub async fn fetch_wp_translation_providers_for_session_with_signing_key(
    client: &Client,
    server_base: &str,
    session_token: &str,
    trusted_public_key_pem: Option<&str>,
) -> anyhow::Result<Vec<WpTranslationProviderItem>> {
    let mut headers = HeaderMap::new();
    headers.insert("X-Client-Session", session_token.parse()?);
    let list_url = format!("{}/api/v1/client/wp-translation-providers", server_base);
    let list_raw: Value = crate::auth::request_json_encrypted_with_signing_key(
        client.get(list_url).headers(headers),
        "client wp-translation-providers",
        session_token,
        trusted_public_key_pem,
    )
    .await?;
    let mut items = if let Ok(resp) =
        serde_json::from_value::<ApiResponse<WpTranslationProvidersPagedData>>(list_raw.clone())
    {
        resp.data.items
    } else if let Ok(data) =
        serde_json::from_value::<WpTranslationProvidersPagedData>(list_raw.clone())
    {
        data.items
    } else {
        anyhow::bail!(
            "client wp-translation-providers: unsupported response shape ({})",
            list_raw
        );
    };
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(items)
}

#[allow(dead_code)]
pub async fn fetch_cloud_api_types_for_session(
    client: &Client,
    server_base: &str,
    session_token: &str,
) -> anyhow::Result<Vec<CloudApiTypeItem>> {
    fetch_cloud_api_types_for_session_with_signing_key(client, server_base, session_token, None)
        .await
}

pub async fn fetch_cloud_api_types_for_session_with_signing_key(
    client: &Client,
    server_base: &str,
    session_token: &str,
    trusted_public_key_pem: Option<&str>,
) -> anyhow::Result<Vec<CloudApiTypeItem>> {
    let mut headers = HeaderMap::new();
    headers.insert("X-Client-Session", session_token.parse()?);
    let list_url = format!("{}/api/v1/client/cloud-api-types", server_base);
    let list_raw: Value = crate::auth::request_json_encrypted_with_signing_key(
        client.get(list_url).headers(headers),
        "client cloud-api-types",
        session_token,
        trusted_public_key_pem,
    )
    .await?;
    let mut items = if let Ok(resp) =
        serde_json::from_value::<ApiResponse<CloudApiTypesPagedData>>(list_raw.clone())
    {
        resp.data.items
    } else if let Ok(data) = serde_json::from_value::<CloudApiTypesPagedData>(list_raw.clone()) {
        data.items
    } else {
        anyhow::bail!(
            "client cloud-api-types: unsupported response shape ({})",
            list_raw
        );
    };
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(items)
}

pub(crate) async fn fetch_components_for_session(
    client: &Client,
    server_base: &str,
    session_token: &str,
) -> anyhow::Result<Vec<ComponentItem>> {
    let mut headers = HeaderMap::new();
    headers.insert("X-Client-Session", session_token.parse()?);
    let mut items: Vec<ComponentItem> = Vec::new();
    let mut page: usize = 1;
    let per_page: usize = 200;
    let mut total_pages: usize = 1;

    loop {
        let list_url = format!(
            "{}/api/v1/client/components?status=active&page={}&per_page={}&client_type=wpplugin",
            server_base, page, per_page
        );
        let list_raw: Value = request_json_encrypted(
            client.get(list_url).headers(headers.clone()),
            "client components",
            session_token,
        )
        .await?;

        let (mut page_items, page_total_pages) = if let Ok(resp) =
            serde_json::from_value::<ApiResponse<ComponentsData>>(list_raw.clone())
        {
            (resp.data.items, resp.data.total_pages.unwrap_or(1).max(1))
        } else if let Ok(data) = serde_json::from_value::<ComponentsData>(list_raw.clone()) {
            (data.items, data.total_pages.unwrap_or(1).max(1))
        } else {
            anyhow::bail!(
                "client components: unsupported response shape ({})",
                list_raw
            );
        };

        items.append(&mut page_items);
        total_pages = total_pages.max(page_total_pages);
        if page >= total_pages {
            break;
        }
        page += 1;
    }

    items.sort_by(|a, b| a.id.cmp(&b.id));
    items.dedup_by(|a, b| a.id == b.id);
    Ok(items)
}

#[cfg(test)]
mod tests;

pub(crate) fn read_recent_log_lines(path: &str, limit: usize) -> anyhow::Result<Vec<String>> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("read log file failed: {}", path))?;
    let mut lines: Vec<String> = raw.lines().map(|line| line.to_string()).collect();
    if lines.len() > limit {
        let start = lines.len().saturating_sub(limit);
        lines = lines[start..].to_vec();
    }
    Ok(lines)
}
