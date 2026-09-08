use anyhow::{anyhow, Context};
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::auth::{
    build_request_id, http_status_line, parse_api_error_response, request_json_encrypted,
    UpstreamApiError,
};
use crate::bindings::{
    load_components_local, load_proxy_profiles, load_vendor_keys, load_vendor_oauth,
    parse_business_line_task_type_binding_key, save_component_bindings, save_components_local,
    save_domain_token_bindings, save_rule_component_bindings, save_task_type_component_bindings,
};
use crate::config::REQUEST_ID_HEADER;
use crate::crypto::resolve_download_template_json;
use crate::logging::{snippet, unix_ts};
use crate::types::*;
use crate::web_ui::fetch_components_for_session;

mod catalog;
mod downloads;
mod helpers;
mod local_registry;
mod rule_discovery;
mod testing;
mod versions;

pub(super) use self::catalog::{
    handle_components_capabilities, handle_components_refresh, handle_server_components_search,
};
pub(super) use self::downloads::{
    fetch_signing_key_from_server, handle_components_template, is_signature_related_error,
    persist_signing_key_material,
};
use self::downloads::{
    request_component_download_with_passthrough, resolve_download_template_with_signing_key,
};
#[cfg(test)]
pub(super) use self::helpers::{
    backfill_local_components_from_server, invalid_task_override_paths,
    patch_template_snapshot_for_local_kind,
};
pub(super) use self::helpers::{
    build_local_component_runtime_for_task, component_exists_in_local_doc,
    find_server_component_by_template_id, load_local_components_runtime_doc,
    local_component_capability_id, normalized_vendor_id, save_component_bindings_runtime_doc,
    save_domain_token_bindings_runtime_doc, save_local_components_runtime_doc,
    save_rule_component_bindings_runtime_doc, save_task_type_component_bindings_runtime_doc,
    sync_local_components_from_server, validate_component_binding_vendor_alignment,
    validate_editable_overrides_for_component_id, validate_local_component_runtime_ready_for_task,
    validate_server_component_api_version, validate_task_editable_overrides,
};
use self::helpers::{
    collect_component_usage_summary, component_in_use_message, ensure_local_component_snapshot,
    fetch_server_template_snapshot_for_local_component,
    refresh_local_component_snapshot_from_server,
    validate_local_component_api_version_with_cached_components,
};
#[cfg(test)]
pub(super) use self::local_registry::generate_openai_compatible_template;
pub(super) use self::local_registry::{
    handle_install_server_template_to_local, handle_local_component_create_v2,
    handle_local_component_delete_v2, handle_local_component_detail, handle_local_component_export,
    handle_local_component_import, handle_local_component_refresh_snapshot,
    handle_local_component_update_v2, handle_local_components_list,
};
pub(super) use self::rule_discovery::handle_rule_component_binding_discovery;
pub(super) use self::testing::{
    handle_local_component_quick_test, handle_local_component_test,
    handle_local_component_test_file,
};
pub(super) use self::versions::{
    handle_component_version_create, handle_component_version_delete,
    handle_component_version_test, handle_component_version_update,
};
use super::errors::{
    maybe_write_upstream_api_error, write_conflict_response, write_error_response,
    write_error_response_with_status, write_not_found_response, write_session_required,
};
use super::http::{parse_query_string, write_http_response};
use super::{
    components_local_path, proxy_profiles_path, update_state_error, vendor_keys_path,
    vendor_oauth_path, web_ui_sqlite_storage_enabled,
};
