#![allow(dead_code, unused)]
/// Shared production core consumed by the independent WebUI and Desktop clients.
pub mod component_rt;

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
pub mod i18n; // pub: client-desktop tray menu consumes the same catalog (X-6)
pub(crate) mod logging;
pub(crate) mod oauth;
pub(crate) mod persistence;
pub(crate) mod resource_governor;
pub(crate) mod retained_assets;
pub(crate) mod storage_capacity;
pub mod sync_engine;
pub mod task_engine;
pub mod web_ui;
pub mod worker;
