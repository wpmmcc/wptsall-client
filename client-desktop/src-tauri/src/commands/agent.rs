use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;

static AGENT_TOKEN: std::sync::OnceLock<CancellationToken> = std::sync::OnceLock::new();

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
pub async fn start_agent(handle: AppHandle) -> anyhow::Result<()> {
    let token = shutdown_token().clone();
    tracing::info!("Desktop agent starting...");

    // Persistent state must not depend on the GUI process CWD (BUG-RT-01).
    ensure_data_dir(&handle);

    // Pin WebUI SQLite storage before the shared runtime starts. Desktop does
    // not go through CLI `WPTSALL_WEB_UI=1`; without this, UI and worker can
    // disagree on JSON vs SQLite and installed components disappear.
    wptsall_client::web_ui::ensure_web_ui_storage_mode();

    let result = wptsall_client::web_ui::run_web_ui(token, std::time::Instant::now()).await;
    tracing::info!("Desktop agent shut down.");
    result
}
