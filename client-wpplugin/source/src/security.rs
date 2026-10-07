//! Client security bootstrap — wraps `wptsall-client-security` for WebUI/Desktop.

use anyhow::Result;
use std::path::PathBuf;
use std::sync::OnceLock;
use wptsall_client_security::{SecurityConfig, SecurityGate};

static GATE: OnceLock<SecurityGate> = OnceLock::new();

pub fn init(app_id: &str, product_id: &str, version: &str) -> Result<&'static SecurityGate> {
    if GATE.get().is_some() {
        return Ok(GATE.get().unwrap());
    }

    let data_dir = std::env::var("WPTSALL_DATA_DIR").unwrap_or_else(|_| default_data_dir(app_id));
    // The website/control-plane is a legacy/test opt-in. An unset URL must
    // stay empty so startup never attempts to contact the official site.
    // Both documented aliases are honored (WPTSALL_SERVER_BASE first).
    let api_base = crate::config::configured_server_base();

    let config = SecurityConfig {
        app_id: app_id.to_string(),
        product_id: product_id.to_string(),
        client_version: version.to_string(),
        data_dir: PathBuf::from(data_dir),
        api_base_url: api_base,
    };

    let binary = std::env::current_exe().ok();
    let gate = SecurityGate::bootstrap(config, binary.as_deref())?;
    GATE.set(gate)
        .map_err(|_| anyhow::anyhow!("security gate already initialized"))?;
    Ok(GATE.get().unwrap())
}

pub fn gate() -> Option<&'static SecurityGate> {
    GATE.get()
}

fn default_data_dir(app_id: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        return format!("{}/.local/share/{}", home.to_string_lossy(), app_id);
    }
    format!("/tmp/{app_id}")
}

/// Register device with server (best-effort, non-fatal on network error).
///
/// P0-LF-03 5.6: device registration is a LEGACY-lane operation. Local mode
/// makes ZERO outbound requests even when `WPTSALL_SERVER_BASE` /
/// `WPTSALL_SERVER_URL` are configured — configuring a URL alone never
/// enables the legacy lane.
pub async fn register_device_if_online() {
    if !device_registration_allowed() {
        eprintln!("[security] device registration skipped: local mode makes no outbound requests");
        return;
    }
    let Some(gate) = gate() else { return };
    if gate.config().api_base_url.trim().is_empty() {
        eprintln!("[security] device registration skipped: no explicit server URL configured");
        return;
    }
    let http = reqwest::Client::new();
    if let Err(e) = gate.register_device(&http).await {
        eprintln!("[security] device registration skipped: {e:#}");
    }
}

/// P0-LF-03 5.6: device registration may only dial in explicit legacy mode.
/// Local mode makes ZERO outbound requests even when
/// `WPTSALL_SERVER_BASE`/`WPTSALL_SERVER_URL` are configured.
fn device_registration_allowed() -> bool {
    crate::config::server_control_plane_enabled()
}

/// P0-LF-03 5.6: the production startup security phase.
///
/// - `WPTSALL_SKIP_SECURITY=1` is a debug-only escape hatch; only an explicit
///   true value skips the phase and it must never appear in M1/M2 evidence.
/// - Security bootstrap failure is FAIL-CLOSED: the process exits non-zero
///   instead of continuing with a broken or missing security gate.
/// - Device registration only fires in legacy mode (zero requests in local
///   mode).
pub async fn startup_security_phase() {
    // S11 (batch G): the valve is honored only in debug builds (guide-16
    // release-forbidden contract); release builds log-and-ignore it.
    if wptsall_client_security::bypass::debug_only_valve("WPTSALL_SKIP_SECURITY") {
        return;
    }
    if let Err(e) = init(
        "wptsall-client",
        crate::updater::PRODUCT_ID,
        env!("CARGO_PKG_VERSION"),
    ) {
        // Fail-closed: never continue with a broken security gate.
        eprintln!("[security] bootstrap failed: {e:#}");
        std::process::exit(2);
    }
    register_device_if_online().await;
}
