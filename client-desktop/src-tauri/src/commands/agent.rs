use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;

static AGENT_TOKEN: std::sync::OnceLock<CancellationToken> = std::sync::OnceLock::new();

/// WebUI keeps 8977. Desktop uses a different loopback port so both products
/// can be installed and running on one machine.
pub const DESKTOP_AGENT_PORT: u16 = 8978;

static USER_PINNED_PORT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Record whether the operator chose a port, then pin the desktop default.
/// Call this before the window is created. A later bind failure may still
/// move off 8978; an operator-supplied port is left unchanged.
pub fn pin_desktop_default_port() {
    let pinned = user_pinned_web_ui_port();
    let _ = USER_PINNED_PORT.set(pinned);
    if pinned {
        return;
    }
    std::env::set_var(
        "WPTSALL_WEB_UI_BIND",
        format!("127.0.0.1:{DESKTOP_AGENT_PORT}"),
    );
    std::env::set_var("WPTSALL_WEB_UI_PORT", DESKTOP_AGENT_PORT.to_string());
}

fn operator_pinned_port() -> bool {
    *USER_PINNED_PORT.get().unwrap_or(&false)
}

/// How many times the embedded agent may retry on a fresh loopback port
/// after a bind conflict before surfacing the failure.
const MAX_BIND_RETRIES: u8 = 3;

pub fn shutdown_token() -> &'static CancellationToken {
    AGENT_TOKEN.get_or_init(CancellationToken::new)
}

/// Pin the agent's persistent state to the OS application-data directory.
///
/// BUG-RT-01: the shared runtime's default paths are CWD-relative
/// (`./runtime`, `./logs`, `./config`). GUI launches get no useful CWD —
/// macOS Finder/Launchpad starts apps with CWD `/` (a read-only volume, so
/// SQLite bootstrap fails with EPERM/EROFS), and Windows shortcuts without
/// a start-in folder may start in System32. Setting WPTSALL_DATA_DIR (when
/// the user has not already set it) makes the core resolve every default
/// under the Tauri app-data dir instead. Explicit per-key env overrides
/// still win inside the core resolver.
fn ensure_data_dir(handle: &AppHandle) {
    let existing = std::env::var("WPTSALL_DATA_DIR")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    if existing {
        return;
    }
    match handle.path().app_data_dir() {
        Ok(dir) => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!("desktop data dir {}: {e:#}", dir.display());
            }
            std::env::set_var("WPTSALL_DATA_DIR", &dir);
            tracing::info!("Desktop agent data dir: {}", dir.display());
        }
        Err(e) => {
            // No identifier-derived data dir (misconfigured bundle). Keep the
            // legacy CWD-relative behavior but say so loudly.
            tracing::warn!("app_data_dir unavailable, falling back to CWD-relative paths: {e:#}");
        }
    }
}

/// Start the embedded agent (same logic as the WebUI client main, but
/// managed by Tauri lifecycle). The agent runs the shared WebUI runtime
/// in-process so Desktop commands use the same Protocol v2 worker,
/// SQLite persistence, component bindings, and relation-scoped outbox path
/// as the standalone WebUI client.
///
/// Port-conflict resilience: Desktop starts on 8978 so it does not take the
/// WebUI port. If that port is already taken and the operator did not pin
/// one, retry on the next free loopback port. An explicit port that cannot
/// bind must fail loudly, not silently drift.
pub async fn start_agent(handle: AppHandle) -> anyhow::Result<()> {
    let token = shutdown_token().clone();
    tracing::info!("Desktop agent starting...");

    // Persistent state must not depend on the GUI process CWD (BUG-RT-01).
    ensure_data_dir(&handle);

    // Pin WebUI SQLite storage before the shared runtime starts. Desktop does
    // not go through CLI `WPTSALL_WEB_UI=1`; without this, UI and worker can
    // disagree on JSON vs SQLite and installed components disappear.
    wptsall_client::web_ui::ensure_web_ui_storage_mode();

    let user_pinned_port = operator_pinned_port();
    let mut bind_conflicts = 0u8;
    loop {
        let result =
            wptsall_client::web_ui::run_web_ui(token.clone(), std::time::Instant::now()).await;
        match result {
            Ok(()) => {
                tracing::info!("Desktop agent shut down.");
                return Ok(());
            }
            Err(err) => {
                let chain = format!("{err:#}");
                if chain.contains("bind web ui failed")
                    && !user_pinned_port
                    && bind_conflicts < MAX_BIND_RETRIES
                {
                    bind_conflicts += 1;
                    match pick_free_loopback_port() {
                        Ok(port) => {
                            tracing::warn!(
                                "desktop agent default port is busy; retrying on 127.0.0.1:{port}"
                            );
                            std::env::set_var("WPTSALL_WEB_UI_BIND", format!("127.0.0.1:{port}"));
                            std::env::set_var("WPTSALL_WEB_UI_PORT", port.to_string());
                            continue;
                        }
                        Err(port_err) => {
                            tracing::error!("could not pick a fallback port: {port_err:#}");
                        }
                    }
                }
                tracing::error!("Desktop agent failed: {err:#}");
                return Err(err);
            }
        }
    }
}

/// True when the user explicitly pinned the embedded WebUI address/port via
/// env. Explicit configuration must not be silently overridden by the
/// bind-conflict fallback.
fn user_pinned_web_ui_port() -> bool {
    std::env::var("WPTSALL_WEB_UI_BIND")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
        || std::env::var("WPTSALL_WEB_UI_PORT")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
}

/// Reserve-and-release an ephemeral loopback port for the fallback bind.
fn pick_free_loopback_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    Ok(port)
}
