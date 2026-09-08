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
    if crate::config::env_bool("WPTSALL_SKIP_SECURITY", false) {
        eprintln!("[security] WPTSALL_SKIP_SECURITY=1 — debug-only, security bootstrap skipped");
        return;
    }
    if let Err(e) = init("wptsall-client", crate::updater::PRODUCT_ID, env!("CARGO_PKG_VERSION")) {
        // Fail-closed: never continue with a broken security gate.
        eprintln!("[security] bootstrap failed: {e:#}");
        std::process::exit(2);
    }
    register_device_if_online().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TestEnvVarGuard;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    async fn spawn_recording_canary() -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        tokio::spawn(async move {
            loop {
                let Ok((_socket, _)) = listener.accept().await else {
                    return;
                };
                counter_clone.fetch_add(1, Ordering::SeqCst);
                // Drop immediately: the peer sees a broken response, which is
                // fine because registration is best-effort.
            }
        });
        (format!("http://{}", addr), counter)
    }

    /// P0-LF-03 5.6 startup canary: BOTH server-base aliases point at
    /// recording canaries. The startup security phase in local mode makes
    /// ZERO outbound requests (device registration included); only an
    /// explicit legacy opt-in produces a request.
    #[tokio::test]
    async fn startup_security_phase_local_mode_makes_zero_outbound_requests() {
        // NOTE: TestEnvVarGuard::set already takes the process env lock
        // (depth-counted); a manual lock here would self-deadlock.
        let (base_url, base_counter) = spawn_recording_canary().await;
        let (alias_url, alias_counter) = spawn_recording_canary().await;
        let _base_guard = TestEnvVarGuard::set("WPTSALL_SERVER_BASE", &base_url);
        let _alias_guard = TestEnvVarGuard::set("WPTSALL_SERVER_URL", &alias_url);
        let _mode_guard = TestEnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());

        // The startup decision: local mode never dials, whatever URLs say.
        assert!(
            !device_registration_allowed(),
            "configuring server URLs alone must not enable device registration"
        );
        register_device_if_online().await;
        assert_eq!(
            base_counter.load(Ordering::SeqCst),
            0,
            "WPTSALL_SERVER_BASE must not receive startup traffic in local mode"
        );
        assert_eq!(
            alias_counter.load(Ordering::SeqCst),
            0,
            "WPTSALL_SERVER_URL must not receive startup traffic in local mode"
        );

        // Legacy opt-in counterpart: registration is allowed to dial (a real
        // gate dials the canary; here the decision gate is what is pinned).
        let _legacy_guard = TestEnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
        assert!(device_registration_allowed());
        let config = wptsall_client_security::SecurityConfig {
            app_id: "wptsall-client-canary-test".to_string(),
            product_id: "wptsall-client".to_string(),
            client_version: "0.0.0-test".to_string(),
            data_dir: std::env::temp_dir().join(format!(
                "wptsall-security-canary-{}",
                uuid::Uuid::new_v4()
            )),
            api_base_url: base_url,
        };
        let gate = wptsall_client_security::SecurityGate::bootstrap(config, None)
            .expect("security gate bootstrap for canary test");
        let http = reqwest::Client::new();
        let _ = gate.register_device(&http).await; // best-effort by design
        assert!(
            base_counter.load(Ordering::SeqCst) > 0,
            "in legacy mode the device registration must reach the configured canary"
        );
        assert_eq!(
            alias_counter.load(Ordering::SeqCst),
            0,
            "only the resolved server base is used"
        );
        let _ = std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("wptsall-security-canary-lookup")),
        );
    }

    /// P0-LF-03 5.6: sign-plugin auto-download and update checks must never
    /// sit on the startup path. The auto-download lives in the legacy worker
    /// (`run_server_worker`) and update checks are explicit route actions.
    #[test]
    fn sign_plugin_auto_download_is_not_on_startup_paths() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let main_rs = std::fs::read_to_string(format!("{manifest_dir}/src/main.rs"))
            .expect("read main.rs");
        assert!(
            !main_rs.contains("auto_download_sign_plugin"),
            "startup must never trigger sign-plugin auto-download"
        );
        assert!(
            !main_rs.contains("check_for_update"),
            "startup must never trigger update checks"
        );
    }
}
