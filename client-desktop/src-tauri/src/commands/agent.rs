use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

static AGENT_TOKEN: std::sync::OnceLock<CancellationToken> = std::sync::OnceLock::new();

pub fn shutdown_token() -> &'static CancellationToken {
    AGENT_TOKEN.get_or_init(CancellationToken::new)
}

/// Start the embedded agent (same logic as the WebUI client main, but
/// managed by Tauri lifecycle). The agent runs the shared WebUI runtime
/// in-process so Desktop commands use the same Protocol v2 worker,
/// SQLite persistence, component bindings, and relation-scoped outbox path
/// as the standalone WebUI client.
pub async fn start_agent(handle: AppHandle) -> anyhow::Result<()> {
    let token = shutdown_token().clone();
    tracing::info!("Desktop agent starting...");

    // Store app handle for future notification access; the shared runtime owns
    // worker state and persistence for now.
    let _ = handle;

    // Pin WebUI SQLite storage before the shared runtime starts. Desktop does
    // not go through CLI `WPTSALL_WEB_UI=1`; without this, UI and worker can
    // disagree on JSON vs SQLite and installed components disappear.
    wptsall_client::web_ui::ensure_web_ui_storage_mode();

    let result = wptsall_client::web_ui::run_web_ui(token, std::time::Instant::now()).await;
    tracing::info!("Desktop agent shut down.");
    result
}
