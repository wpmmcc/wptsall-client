mod auth;
mod bindings;
mod component_rt;
mod config;
mod contract_capabilities;
mod crypto;
pub(crate) mod db;
mod i18n;
mod logging;
mod oauth;
mod persistence;
mod resource_governor;
mod security;
mod task_engine;
mod types;
mod updater;
mod web_ui;
mod worker;

use config::env_bool;
use tokio_util::sync::CancellationToken;
use wptsall_i18n::{I18nManager, Language};

static I18N_EN: &str = include_str!("../locales/en.json");
static I18N_ZH_CN: &str = include_str!("../locales/zh-CN.json");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // P0-LF-03 5.6: startup security phase is FAIL-CLOSED — a bootstrap
    // failure exits non-zero instead of continuing. Device registration only
    // fires in legacy mode; local mode makes zero outbound requests.
    security::startup_security_phase().await;

    // Initialize i18n from embedded catalogs. Runtime binaries can execute from
    // an installed prefix, a temporary OTA staging dir, or the source tree; they
    // must not depend on the process cwd matching `../../locales`.
    I18nManager::init_with_content(I18N_EN, I18N_ZH_CN, Language::En)
        .map_err(|e| anyhow::anyhow!("Failed to initialize i18n: {}", e))?;

    let shutdown_token = CancellationToken::new();
    let start_time = std::time::Instant::now();

    // Spawn signal listener: SIGINT (Ctrl+C) + SIGTERM
    {
        let token = shutdown_token.clone();
        tokio::spawn(async move {
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            let terminate = async {
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM handler")
                    .recv()
                    .await;
            };
            #[cfg(not(unix))]
            let terminate = std::future::pending::<()>();

            tokio::select! {
                _ = ctrl_c => {
                    eprintln!("\nReceived SIGINT, shutting down gracefully...");
                }
                _ = terminate => {
                    eprintln!("\nReceived SIGTERM, shutting down gracefully...");
                }
            }
            token.cancel();
        });
    }

    if env_bool("WPTSALL_WEB_UI", false) {
        return web_ui::run_web_ui(shutdown_token, start_time).await;
    }
    worker::run_worker_cli(shutdown_token).await
}
