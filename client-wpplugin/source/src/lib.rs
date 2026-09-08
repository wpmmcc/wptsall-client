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
