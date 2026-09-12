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

    // `--webui`: explicit CLI mode switch used by the Windows startup
    // shortcut (BUG-INS-02). A .lnk cannot carry environment variables, so
    // the installer passes this flag instead of relying on WPTSALL_WEB_UI=1
    // being present in the launching context.
    if std::env::args().any(|arg| arg == "--webui") {
        std::env::set_var("WPTSALL_WEB_UI", "1");
    }

    if env_bool("WPTSALL_WEB_UI", false) {
        return web_ui::run_web_ui(shutdown_token, start_time).await;
    }
    worker::run_worker_cli(shutdown_token).await
}

#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-main-rs
    // oracle: L1
    // Binary-root wiring contract: main() bootstraps i18n from the two
    // embedded catalogs via I18nManager::init_with_content; the test
    // replays that exact bootstrap and resolves a known key in both
    // languages through the global instance it installs.
    use super::*;

    #[test]
    fn embedded_catalogs_initialize_the_i18n_manager() {
        // main()'s observable bootstrap contract: both embedded catalogs
        // parse, initialize the global manager, and RESOLVE real keys.
        //
        // The former flat/nested mismatch (documented 2026-09-12: catalogs
        // use flat dotted keys while translate() walked nested objects, so
        // every lookup KeyNotFound) was resolved in wptsall-i18n: the
        // walker now runs the nested walk first and falls back to the
        // whole dotted key as a literal member. Both catalog styles
        // resolve with one manager.
        I18nManager::init_with_content(I18N_EN, I18N_ZH_CN, Language::En)
            .expect("embedded en/zh-CN catalogs must initialize the manager");
        assert!(
            I18nManager::get().is_ok(),
            "the global manager must be installed after bootstrap"
        );

        // Real translation roundtrip through the embedded flat catalogs:
        // "app.title" exists in both languages.
        let manager = I18nManager::get().expect("global manager installed");
        let en = manager.translate("app.title", Language::En);
        assert!(en.is_ok(), "embedded en catalog must resolve app.title: {:?}", en.err());
        let zh = manager.translate("app.title", Language::ZhCn);
        assert!(zh.is_ok(), "embedded zh-CN catalog must resolve app.title: {:?}", zh.err());
        assert_ne!(
            en.unwrap(),
            zh.unwrap(),
            "the two embedded catalogs must actually differ for the probe key"
        );
    }
}
