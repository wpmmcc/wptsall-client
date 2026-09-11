pub mod commands;
pub mod security;

use client_runtime_core::env_helpers::env_bool;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wptsall_desktop=info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_notification::init())
        // Persist window size/position across restarts (per the tauri
        // window-state plugin; state file lives in the app data dir).
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            // Phase 5: security bootstrap for desktop client.
            // Only an explicit true value skips security; `WPTSALL_SKIP_SECURITY=0`
            // must keep the production verification gate active.
            if !env_bool("WPTSALL_SKIP_SECURITY", false) {
                if let Err(e) = security::init_desktop() {
                    tracing::warn!("security bootstrap: {e:#}");
                }
            }

            let handle = app.handle().clone();
            // Spawn the agent background service
            tauri::async_runtime::spawn(async move {
                if let Err(e) = commands::agent::start_agent(handle).await {
                    tracing::error!("Agent failed to start: {e}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::auth::login,
            commands::auth::start_oauth,
            commands::auth::logout,
            commands::auth::get_status,
            commands::auth::refresh_domains,
            commands::sites::list_sites,
            commands::sites::add_site,
            commands::sites::import_site_connection,
            commands::sites::remove_site,
            commands::sites::test_connection,
            commands::tasks::list_tasks,
            commands::tasks::trigger_discover,
            commands::tasks::get_task_detail,
            commands::tasks::focus_discovery_relation,
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::components::list_components,
            commands::components::configure_component,
            commands::components::configure_mock_component,
            commands::components::quick_test_component,
            commands::components::create_component_version,
            commands::components::upsert_rule_component_binding,
            commands::integrations::list_provider_catalog,
            commands::integrations::refresh_provider_catalog,
            commands::integrations::install_catalog_template,
            commands::integrations::export_integration_pack,
            commands::integrations::preview_integration_pack,
            commands::integrations::import_integration_pack,
            commands::worker::get_worker_status,
            commands::worker::start_worker,
            commands::worker::stop_worker,
            commands::worker::configure_worker,
            commands::worker::run_worker_once,
            commands::apikeys::list_keys,
            commands::apikeys::save_key,
            commands::apikeys::delete_key,
            // P1-G: vendor keys / OAuth / proxy profiles proxied through the
            // embedded WebUI runtime (closes the P1-12 Desktop gap).
            commands::apikeys::list_vendor_keys,
            commands::apikeys::create_vendor_key,
            commands::apikeys::update_vendor_key,
            commands::apikeys::delete_vendor_key,
            commands::apikeys::list_vendor_oauth,
            commands::apikeys::create_vendor_oauth,
            commands::apikeys::update_vendor_oauth,
            commands::apikeys::authorize_vendor_oauth,
            commands::apikeys::delete_vendor_oauth,
            commands::apikeys::list_proxy_profiles,
            commands::apikeys::create_proxy_profile,
            commands::apikeys::update_proxy_profile,
            commands::apikeys::test_proxy_profile,
            commands::apikeys::delete_proxy_profile,
            commands::review::list_translations,
            commands::review::retry_translation,
            commands::review::batch_retry_translations,
            commands::review::batch_delete_translations,
            commands::review::update_translation,
            commands::review::list_jobs,
            commands::review::get_job,
            commands::review::list_job_items,
            commands::review::get_item_content,
            commands::review::save_item_translated,
            commands::review::approve_item,
            commands::review::resubmit_item,
            commands::review::retranslate_item,
            commands::logs::get_logs,
            commands::logs::get_recent_logs,
            commands::platform::get_platform_info,
            commands::update::check_for_update,
            commands::update::perform_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running application");
}
