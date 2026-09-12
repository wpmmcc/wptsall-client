#![allow(dead_code, unused)]
/// Library entry point — exposes types needed by integration tests in tests/.
/// Only the modules transitively required by key_pool and oauth are declared here.
pub mod component_rt;
pub use crate::web_ui::test_support;
pub mod types;
pub use crate::web_ui::{
    fetch_cloud_api_types_for_session, fetch_wp_translation_providers_for_session,
};
pub mod security;
pub mod updater;

// Transitive dependencies of component_rt modules (kept internal).
pub(crate) mod auth;
pub(crate) mod bindings;
pub(crate) mod config;
pub(crate) mod contract_capabilities;
pub(crate) mod crypto;
pub(crate) mod db;
pub(crate) mod i18n;
pub(crate) mod logging;
pub(crate) mod oauth;
pub(crate) mod persistence;
pub(crate) mod resource_governor;
pub mod task_engine;
pub mod web_ui;
pub mod worker;

#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-lib-rs
    // oracle: L1
    // Crate-root wiring contract: the documented public surface (i18n
    // catalog lookups, the test_support re-export, the types module) must
    // be reachable through `wptsall_client::` paths as external test
    // targets in tests/ expect them.
    use crate::i18n::t;
    use crate::test_support::WebUiTestHarness;
    use crate::types::KeySelectionStrategy;

    #[test]
    fn crate_root_exposes_documented_surface() {
        assert_eq!(t("app.title", "en"), "WPTSALL Client");
        // The types module re-exports the shared component surface.
        let _strategy: KeySelectionStrategy = KeySelectionStrategy::default();
    }

    #[tokio::test]
    async fn test_support_reexport_builds_the_documented_harness() {
        // The re-exported harness must construct with the documented
        // (server_base, session) shape — the same entry the external test
        // targets in tests/ use.
        let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
            .await
            .expect("harness constructor must be callable through the re-export");
        assert!(
            harness.db_path.to_string_lossy().len() > 0,
            "harness must allocate its per-test storage"
        );
    }
}
