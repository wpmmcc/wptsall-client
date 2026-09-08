use anyhow::{anyhow, Context};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::auth::wp_request_with_transport;
use crate::bindings::{
    build_wp_base_url, load_vendor_keys, load_vendor_oauth, normalize_domain_base,
    parse_business_line_key, parse_rule_component_slot_binding_key, parse_task_type_binding_key,
};
use crate::logging::{session_token_prefix, unix_ts};
use crate::types::*;
use crate::web_ui::{fetch_components_for_session, local_sites_from_domain_token_bindings};

use super::errors::{
    find_passthrough_http_error, maybe_write_upstream_api_error, write_error_response,
    write_error_response_with_status,
};
use super::http::write_http_response;
use super::{
    component_exists_in_local_doc, find_server_component_by_template_id,
    load_local_components_runtime_doc, local_component_capability_id, normalized_vendor_id,
    resolve_effective_route_secret_for_domain, save_component_bindings_runtime_doc,
    save_domain_token_bindings_runtime_doc, save_local_components_runtime_doc,
    save_rule_component_bindings_runtime_doc, save_task_type_component_bindings_runtime_doc,
    validate_component_binding_vendor_alignment, validate_local_component_runtime_ready_for_task,
    validate_server_component_api_version, vendor_keys_path, vendor_oauth_path,
};

pub(super) async fn handle_bindings_upsert(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiUpsertBindingRequest = serde_json::from_slice(body)
        .with_context(|| "invalid /api/components/bindings/upsert json payload")?;
    let component_id = req.component_id.unwrap_or_default().trim().to_string();
    if component_id.is_empty() {
        return write_error_response(socket, "INVALID_COMPONENT_ID", "component_id is required")
            .await;
    }
    let mut auth = req.auth.unwrap_or_default();
    auth.retain(|k, _| !k.trim().is_empty());
    let (path, mut bindings_doc) = {
        let guard = state.lock().await;
        (
            guard.component_bindings_path.clone(),
            guard.component_bindings.clone(),
        )
    };
    let key_ids = req.key_ids;
    let oauth_ids = req.oauth_ids;
    let auth_strategy = req
        .auth_strategy
        .unwrap_or(crate::types::KeySelectionStrategy::RoundRobin);
    let constraints_override = req.constraints_override;
    let request_overrides = req.request_overrides;
    let default_values_override = req.default_values_override;
    let binding_constraints_override = constraints_override.clone();
    let binding_request_overrides = request_overrides.clone();
    let binding_default_values_override = default_values_override.clone();
    // P0-LF-03 5.3: in local mode bindings target LOCAL components only.
    // Component existence, vendor alignment, and API version are validated
    // against local data; the server component cache and
    // `fetch_components_for_session` are never consulted.
    if !crate::config::server_control_plane_enabled()
        && !load_local_components_runtime_doc()
            .components
            .contains_key(&component_id)
    {
        return write_error_response_with_status(
            socket,
            "404 Not Found",
            "COMPONENT_NOT_FOUND",
            &format!("component '{component_id}' not found in local components"),
        )
        .await;
    }
    let local_mode = !crate::config::server_control_plane_enabled();
    let requires_vendor_alignment = !key_ids.is_empty() || !oauth_ids.is_empty();
    let component_vendor_id = if requires_vendor_alignment {
        let local_doc = load_local_components_runtime_doc();
        if let Some(component) = local_doc.components.get(&component_id) {
            normalized_vendor_id(&component.vendor_id)
        } else if local_mode {
            // Unreachable: existence was validated above; kept for exhaustiveness.
            None
        } else {
            let (cached_component, server_base, session_token_opt, client) = {
                let guard = state.lock().await;
                (
                    find_server_component_by_template_id(&guard.components, &component_id).cloned(),
                    guard.server_base.clone(),
                    guard.session_token.clone(),
                    guard.http_client.clone(),
                )
            };
            if let Some(component) = cached_component {
                component
                    .vendor_id
                    .as_deref()
                    .and_then(normalized_vendor_id)
            } else if let Some(session_token) = session_token_opt {
                let components =
                    fetch_components_for_session(&client, &server_base, &session_token).await?;
                let resolved = find_server_component_by_template_id(&components, &component_id)
                    .and_then(|component| component.vendor_id.as_deref())
                    .and_then(normalized_vendor_id);
                let mut guard = state.lock().await;
                guard.components = components;
                guard.updated_at = unix_ts();
                resolved
            } else {
                None
            }
        }
    } else {
        None
    };
    let vendor_keys_doc = load_vendor_keys(&vendor_keys_path()).unwrap_or_default();
    let vendor_oauth_doc = load_vendor_oauth(&vendor_oauth_path()).unwrap_or_default();
    if let Err(err) = validate_component_binding_vendor_alignment(
        &component_id,
        component_vendor_id.as_deref(),
        &key_ids,
        &oauth_ids,
        &vendor_keys_doc,
        &vendor_oauth_doc,
    ) {
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "INVALID_COMPONENT_AUTH_POOL",
            &format!("{:#}", err),
        )
        .await;
    }

    let existing = bindings_doc
        .components
        .get(&component_id)
        .cloned()
        .unwrap_or_default();
    bindings_doc.components.insert(
        component_id.clone(),
        ComponentBindingEntry {
            auth: auth.clone(),
            key_ids: key_ids.clone(),
            oauth_ids: oauth_ids.clone(),
            auth_strategy: auth_strategy.clone(),
            template_id: existing.template_id,
            r#type: existing.r#type,
            name: existing.name,
            language_map: existing.language_map,
            constraints_override: binding_constraints_override.or(existing.constraints_override),
            request_overrides: binding_request_overrides.or(existing.request_overrides),
            default_values_override: binding_default_values_override
                .or(existing.default_values_override),
        },
    );
    let saved_entry = bindings_doc.components.get(&component_id).cloned();

    let mut local_components_doc = load_local_components_runtime_doc();
    let mut local_component_synced = false;
    if let Some(comp) = local_components_doc.components.get_mut(&component_id) {
        comp.component_overrides = Some(ComponentInstanceOverrides {
            constraints_override: constraints_override.clone(),
            request_overrides: request_overrides.clone(),
            default_values_override: default_values_override.clone(),
        });
        comp.updated_at = Some(format!("{}", unix_ts()));
        save_local_components_runtime_doc(&local_components_doc)?;
        local_component_synced = true;
    }
    save_component_bindings_runtime_doc(&path, &bindings_doc)?;
    {
        let mut guard = state.lock().await;
        {
            let db = guard.db.lock().await;
            let _ = crate::db::bindings::save_component_bindings_doc(&db, &bindings_doc);
        }
        guard.component_bindings = bindings_doc;
        guard.last_error.clear();
        guard.last_event = "bindings.upserted".to_string();
        guard.updated_at = unix_ts();
    }
    let payload = json!({
        "success": true,
        "data": {
            "component_id": component_id,
            "auth": auth,
            "key_ids": key_ids,
            "oauth_ids": oauth_ids,
            "auth_strategy": auth_strategy,
            "constraints_override": saved_entry.as_ref().and_then(|e| e.constraints_override.clone()),
            "request_overrides": saved_entry.as_ref().and_then(|e| e.request_overrides.clone()),
            "default_values_override": saved_entry.as_ref().and_then(|e| e.default_values_override.clone()),
            "local_component_synced": local_component_synced
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_bindings_delete(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiDeleteBindingRequest = serde_json::from_slice(body)
        .with_context(|| "invalid /api/components/bindings/delete json payload")?;
    let component_id = req.component_id.unwrap_or_default().trim().to_string();
    if component_id.is_empty() {
        return write_error_response(socket, "INVALID_COMPONENT_ID", "component_id is required")
            .await;
    }
    let (path, mut bindings_doc) = {
        let guard = state.lock().await;
        (
            guard.component_bindings_path.clone(),
            guard.component_bindings.clone(),
        )
    };
    let removed = bindings_doc.components.remove(&component_id).is_some();

    let mut local_components_doc = load_local_components_runtime_doc();
    let mut local_component_synced = false;
    if let Some(comp) = local_components_doc.components.get_mut(&component_id) {
        comp.component_overrides = None;
        comp.updated_at = Some(format!("{}", unix_ts()));
        save_local_components_runtime_doc(&local_components_doc)?;
        local_component_synced = true;
    }
    save_component_bindings_runtime_doc(&path, &bindings_doc)?;
    {
        let mut guard = state.lock().await;
        {
            let db = guard.db.lock().await;
            let _ = crate::db::bindings::save_component_bindings_doc(&db, &bindings_doc);
        }
        guard.component_bindings = bindings_doc;
        guard.last_error.clear();
        guard.last_event = "bindings.deleted".to_string();
        guard.updated_at = unix_ts();
    }
    let payload = json!({
        "success": true,
        "data": {
            "component_id": component_id,
            "deleted": removed,
            "local_component_synced": local_component_synced
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_task_type_components_upsert(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiUpsertTaskTypeComponentRequest = serde_json::from_slice(body)
        .with_context(|| "invalid /api/task-type-components/upsert json payload")?;
    let Some(task_type) = parse_task_type_binding_key(&req.task_type.unwrap_or_default()) else {
        return write_error_response(
            socket,
            "INVALID_TASK_TYPE",
            "task_type must be one of: text,image,video,audio,document,mixed",
        )
        .await;
    };
    let raw_business_line = req
        .business_line
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let business_line = raw_business_line.and_then(parse_business_line_key);
    if raw_business_line.is_some() && business_line.is_none() {
        return write_error_response(
            socket,
            "INVALID_BUSINESS_LINE",
            "business_line must be one of: custom_model,post_content,taxonomy_content,plugin_i18n,config_i18n,theme_i18n,site_strings,menu_strings,widget_strings",
        )
        .await;
    }
    let component_id = req.component_id.unwrap_or_default().trim().to_string();
    if component_id.is_empty() {
        return write_error_response(socket, "INVALID_COMPONENT_ID", "component_id is required")
            .await;
    }
    if let Err(err) = validate_task_type_component_target(
        state,
        &task_type,
        business_line.as_deref(),
        &component_id,
    )
    .await
    {
        let message = format!("{:#}", err);
        // P0-LF-03 5.3: unknown components in local mode surface as
        // COMPONENT_NOT_FOUND instead of a validation error.
        if message.starts_with("COMPONENT_NOT_FOUND") {
            return write_error_response_with_status(
                socket,
                "404 Not Found",
                "COMPONENT_NOT_FOUND",
                &message,
            )
            .await;
        }
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "INVALID_COMPONENT_ID",
            &message,
        )
        .await;
    }
    let (path, mut bindings_doc) = {
        let guard = state.lock().await;
        (
            guard.task_type_component_bindings_path.clone(),
            guard.task_type_component_bindings.clone(),
        )
    };
    if let Some(line) = business_line.clone() {
        let scoped_key = format!("{}:{}", line, task_type);
        bindings_doc.business_line_task_types.insert(
            scoped_key,
            TaskTypeComponentBindingEntry {
                component_id: component_id.clone(),
            },
        );
    } else {
        bindings_doc.task_types.insert(
            task_type.clone(),
            TaskTypeComponentBindingEntry {
                component_id: component_id.clone(),
            },
        );
    }
    save_task_type_component_bindings_runtime_doc(&path, &bindings_doc)?;
    {
        let mut guard = state.lock().await;
        {
            let db = guard.db.lock().await;
            let _ = crate::db::bindings::save_task_type_component_bindings_doc(&db, &bindings_doc);
        }
        guard.task_type_component_bindings = bindings_doc;
        guard.last_error.clear();
        guard.last_event = "task_type_components.upserted".to_string();
        guard.updated_at = unix_ts();
    }
    let payload = json!({
        "success": true,
        "data": {
            "business_line": business_line,
            "task_type": task_type,
            "component_id": component_id
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

async fn validate_task_type_component_target(
    state: &Arc<Mutex<WebUiState>>,
    task_type: &str,
    business_line: Option<&str>,
    component_id: &str,
) -> anyhow::Result<()> {
    let local_doc = load_local_components_runtime_doc();
    if let Some(component) = local_doc.components.get(component_id) {
        let capability_id = local_component_capability_id(&component.kind);
        if capability_id != task_type {
            anyhow::bail!(
                "local component '{}' has kind '{}' and cannot bind to task_type '{}'",
                component_id,
                component.kind,
                task_type
            );
        }
        validate_local_component_runtime_ready_for_task(state, component_id, None).await?;
        return Ok(());
    }

    // P0-LF-03 5.3: local mode validates against local component data only;
    // unknown ids are a local 404-class error, never a website lookup.
    if !crate::config::server_control_plane_enabled() {
        anyhow::bail!(
            "COMPONENT_NOT_FOUND: component '{}' not found in local components",
            component_id
        );
    }

    let (cached_components, server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.components.clone(),
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let mut server_component =
        find_server_component_by_template_id(&cached_components, component_id).cloned();
    if server_component.is_none() {
        if let Some(session_token) = session_token_opt {
            let components =
                fetch_components_for_session(&client, &server_base, &session_token).await?;
            server_component =
                find_server_component_by_template_id(&components, component_id).cloned();
            let mut guard = state.lock().await;
            guard.components = components;
            guard.updated_at = unix_ts();
        }
    }

    let server_component = server_component
        .ok_or_else(|| anyhow::anyhow!("component_id '{}' not found", component_id))?;
    if !server_component
        .status
        .trim()
        .eq_ignore_ascii_case("active")
    {
        anyhow::bail!(
            "server component '{}' is not active (status={})",
            component_id,
            server_component.status
        );
    }
    validate_server_component_api_version(component_id, &server_component)?;
    let component_task_type =
        crate::component_rt::loader::normalize_component_runtime_kind(&server_component.kind)
            .unwrap_or_default();
    if component_task_type != task_type {
        anyhow::bail!(
            "server component '{}' has kind '{}' and cannot bind to task_type '{}'",
            component_id,
            server_component.kind,
            task_type
        );
    }
    if let Some(line) = business_line {
        let supports_business_line = server_component.supported_business_lines.is_empty()
            || server_component
                .supported_business_lines
                .iter()
                .any(|supported| supported == line);
        if !supports_business_line {
            anyhow::bail!(
                "server component '{}' does not support business_line '{}'",
                component_id,
                line
            );
        }
    }
    Ok(())
}

pub(super) async fn handle_task_type_components_delete(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiDeleteTaskTypeComponentRequest = serde_json::from_slice(body)
        .with_context(|| "invalid /api/task-type-components/delete json payload")?;
    let Some(task_type) = parse_task_type_binding_key(&req.task_type.unwrap_or_default()) else {
        return write_error_response(
            socket,
            "INVALID_TASK_TYPE",
            "task_type must be one of: text,image,video,audio,document,mixed",
        )
        .await;
    };
    let raw_business_line = req
        .business_line
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let business_line = raw_business_line.and_then(parse_business_line_key);
    if raw_business_line.is_some() && business_line.is_none() {
        return write_error_response(
            socket,
            "INVALID_BUSINESS_LINE",
            "business_line must be one of: custom_model,post_content,taxonomy_content,plugin_i18n,config_i18n,theme_i18n,site_strings,menu_strings,widget_strings",
        )
        .await;
    }
    let (path, mut bindings_doc) = {
        let guard = state.lock().await;
        (
            guard.task_type_component_bindings_path.clone(),
            guard.task_type_component_bindings.clone(),
        )
    };
    let removed = if let Some(line) = business_line.clone() {
        let scoped_key = format!("{}:{}", line, task_type);
        bindings_doc
            .business_line_task_types
            .remove(&scoped_key)
            .is_some()
    } else {
        bindings_doc.task_types.remove(&task_type).is_some()
    };
    save_task_type_component_bindings_runtime_doc(&path, &bindings_doc)?;
    {
        let mut guard = state.lock().await;
        {
            let db = guard.db.lock().await;
            let _ = crate::db::bindings::save_task_type_component_bindings_doc(&db, &bindings_doc);
        }
        guard.task_type_component_bindings = bindings_doc;
        guard.last_error.clear();
        guard.last_event = "task_type_components.deleted".to_string();
        guard.updated_at = unix_ts();
    }
    let payload = json!({
        "success": true,
        "data": {
            "business_line": business_line,
            "task_type": task_type,
            "deleted": removed
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

fn normalize_rule_binding_scope(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_lowercase();
    match normalized.as_str() {
        "" | "global" => Some("global".to_string()),
        "plugin" => Some("plugin".to_string()),
        "relation" => Some("relation".to_string()),
        "rule" => Some("rule".to_string()),
        _ => None,
    }
}

fn normalize_rule_binding_scope_key(scope: &str, raw: Option<&str>) -> anyhow::Result<String> {
    match scope {
        "global" => Ok(String::new()),
        "plugin" => {
            let key = raw.unwrap_or_default().trim().to_lowercase();
            if key.is_empty() {
                return Err(anyhow!("scope=plugin requires non-empty scope_key"));
            }
            Ok(key)
        }
        "relation" => {
            let key_raw = raw.unwrap_or_default().trim();
            if key_raw.is_empty() {
                return Err(anyhow!("scope=relation requires scope_key (relation_id)"));
            }
            let relation_id = key_raw
                .parse::<u64>()
                .with_context(|| "scope_key must be numeric relation_id")?;
            if relation_id == 0 {
                return Err(anyhow!("scope_key must be positive numeric relation_id"));
            }
            Ok(relation_id.to_string())
        }
        "rule" => {
            let key_raw = raw.unwrap_or_default().trim();
            if key_raw.is_empty() {
                return Err(anyhow!("scope=rule requires scope_key (rule_id)"));
            }
            let rule_id = key_raw
                .parse::<u64>()
                .with_context(|| "scope_key must be numeric rule_id")?;
            if rule_id == 0 {
                return Err(anyhow!("scope_key must be positive numeric rule_id"));
            }
            Ok(rule_id.to_string())
        }
        _ => Err(anyhow!("invalid scope")),
    }
}

pub(super) async fn handle_rule_component_bindings_upsert(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiUpsertRuleComponentBindingRequest = serde_json::from_slice(body)
        .with_context(|| "invalid /api/rule-component-bindings/upsert json payload")?;

    let Some(scope) = normalize_rule_binding_scope(&req.scope.unwrap_or_default()) else {
        return write_error_response(
            socket,
            "INVALID_SCOPE",
            "scope must be one of: global,plugin,relation,rule",
        )
        .await;
    };
    let scope_key = match normalize_rule_binding_scope_key(&scope, req.scope_key.as_deref()) {
        Ok(value) => value,
        Err(_) => {
            return write_error_response(
                socket,
                "INVALID_SCOPE_KEY",
                "scope_key invalid for scope",
            )
            .await;
        }
    };
    let Some(slot_key) = parse_rule_component_slot_binding_key(&req.slot_key.unwrap_or_default())
    else {
        return write_error_response(
            socket,
            "INVALID_SLOT_KEY",
            "slot_key must be one of: plain_text,rich_html,json_structured,serialized_php,media_ref,media_ref:image,media_ref:video,media_ref:audio,media_ref:document,slug,code",
        )
        .await;
    };
    let component_id = req.component_id.unwrap_or_default().trim().to_string();
    if component_id.is_empty() {
        return write_error_response(socket, "INVALID_COMPONENT_ID", "component_id is required")
            .await;
    }
    let local_doc = load_local_components_runtime_doc();
    if !component_exists_in_local_doc(&local_doc, &component_id) {
        return write_error_response(
            socket,
            "COMPONENT_NOT_FOUND",
            &format!(
                "component_id '{}' not found in local components",
                component_id
            ),
        )
        .await;
    }

    let (path, mut bindings_doc) = {
        let guard = state.lock().await;
        (
            guard.rule_component_bindings_path.clone(),
            guard.rule_component_bindings.clone(),
        )
    };

    match scope.as_str() {
        "global" => {
            bindings_doc
                .global_defaults
                .insert(slot_key.clone(), component_id.clone());
        }
        "plugin" => {
            bindings_doc
                .plugin_bindings
                .entry(scope_key.clone())
                .or_default()
                .insert(slot_key.clone(), component_id.clone());
        }
        "relation" => {
            bindings_doc
                .relation_bindings
                .entry(scope_key.clone())
                .or_default()
                .insert(slot_key.clone(), component_id.clone());
        }
        "rule" => {
            bindings_doc
                .rule_bindings
                .entry(scope_key.clone())
                .or_default()
                .insert(slot_key.clone(), component_id.clone());
        }
        _ => {
            return write_error_response(
                socket,
                "INVALID_SCOPE",
                "scope must be one of: global,plugin,relation,rule",
            )
            .await;
        }
    }

    save_rule_component_bindings_runtime_doc(&path, &bindings_doc)?;
    {
        let mut guard = state.lock().await;
        {
            let db = guard.db.lock().await;
            let _ = crate::db::bindings::save_rule_component_bindings_doc(&db, &bindings_doc);
        }
        guard.rule_component_bindings = bindings_doc;
        guard.last_error.clear();
        guard.last_event = "rule_component_bindings.upserted".to_string();
        guard.updated_at = unix_ts();
    }

    let payload = json!({
        "success": true,
        "data": {
            "scope": scope,
            "scope_key": scope_key,
            "slot_key": slot_key,
            "component_id": component_id
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_rule_component_bindings_delete(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiDeleteRuleComponentBindingRequest = serde_json::from_slice(body)
        .with_context(|| "invalid /api/rule-component-bindings/delete json payload")?;

    let Some(scope) = normalize_rule_binding_scope(&req.scope.unwrap_or_default()) else {
        return write_error_response(
            socket,
            "INVALID_SCOPE",
            "scope must be one of: global,plugin,relation,rule",
        )
        .await;
    };
    let scope_key = match normalize_rule_binding_scope_key(&scope, req.scope_key.as_deref()) {
        Ok(value) => value,
        Err(_) => {
            return write_error_response(
                socket,
                "INVALID_SCOPE_KEY",
                "scope_key invalid for scope",
            )
            .await;
        }
    };
    let Some(slot_key) = parse_rule_component_slot_binding_key(&req.slot_key.unwrap_or_default())
    else {
        return write_error_response(
            socket,
            "INVALID_SLOT_KEY",
            "slot_key must be one of: plain_text,rich_html,json_structured,serialized_php,media_ref,media_ref:image,media_ref:video,media_ref:audio,media_ref:document,slug,code",
        )
        .await;
    };

    let (path, mut bindings_doc) = {
        let guard = state.lock().await;
        (
            guard.rule_component_bindings_path.clone(),
            guard.rule_component_bindings.clone(),
        )
    };

    let removed = match scope.as_str() {
        "global" => bindings_doc.global_defaults.remove(&slot_key).is_some(),
        "plugin" => {
            let did_remove = bindings_doc
                .plugin_bindings
                .get_mut(&scope_key)
                .map(|map| map.remove(&slot_key).is_some())
                .unwrap_or(false);
            if let Some(map) = bindings_doc.plugin_bindings.get(&scope_key) {
                if map.is_empty() {
                    bindings_doc.plugin_bindings.remove(&scope_key);
                }
            }
            did_remove
        }
        "relation" => {
            let did_remove = bindings_doc
                .relation_bindings
                .get_mut(&scope_key)
                .map(|map| map.remove(&slot_key).is_some())
                .unwrap_or(false);
            if let Some(map) = bindings_doc.relation_bindings.get(&scope_key) {
                if map.is_empty() {
                    bindings_doc.relation_bindings.remove(&scope_key);
                }
            }
            did_remove
        }
        "rule" => {
            let did_remove = bindings_doc
                .rule_bindings
                .get_mut(&scope_key)
                .map(|map| map.remove(&slot_key).is_some())
                .unwrap_or(false);
            if let Some(map) = bindings_doc.rule_bindings.get(&scope_key) {
                if map.is_empty() {
                    bindings_doc.rule_bindings.remove(&scope_key);
                }
            }
            did_remove
        }
        _ => false,
    };

    save_rule_component_bindings_runtime_doc(&path, &bindings_doc)?;
    {
        let mut guard = state.lock().await;
        {
            let db = guard.db.lock().await;
            let _ = crate::db::bindings::save_rule_component_bindings_doc(&db, &bindings_doc);
        }
        guard.rule_component_bindings = bindings_doc;
        guard.last_error.clear();
        guard.last_event = "rule_component_bindings.deleted".to_string();
        guard.updated_at = unix_ts();
    }

    let payload = json!({
        "success": true,
        "data": {
            "scope": scope,
            "scope_key": scope_key,
            "slot_key": slot_key,
            "deleted": removed
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_domain_tokens_upsert(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiUpsertDomainTokenRequest = serde_json::from_slice(body)
        .with_context(|| "invalid /api/domain-tokens/upsert json payload")?;
    let domain_key = normalize_domain_base(&req.api_base_url.unwrap_or_default());
    let existing_domain_key = normalize_domain_base(&req.existing_api_base_url.unwrap_or_default());
    if domain_key.is_empty() {
        return write_error_response(socket, "INVALID_API_BASE_URL", "api_base_url is required")
            .await;
    }
    let requested_token = req.wp_client_token.unwrap_or_default().trim().to_string();
    let route_secret = req.route_secret.unwrap_or_default().trim().to_string();
    let (path, mut bindings_doc) = {
        let guard = state.lock().await;
        (
            guard.domain_token_bindings_path.clone(),
            guard.domain_token_bindings.clone(),
        )
    };
    let existing_entry = if !existing_domain_key.is_empty() {
        bindings_doc.domains.get(&existing_domain_key)
    } else {
        bindings_doc.domains.get(&domain_key)
    };
    let can_reuse_existing_token = requested_token.is_empty()
        && (!existing_domain_key.is_empty() && existing_domain_key == domain_key
            || existing_domain_key.is_empty());
    let token = if can_reuse_existing_token {
        existing_entry
            .map(|entry| entry.wp_client_token.clone())
            .unwrap_or_default()
    } else {
        requested_token
    };
    if token.is_empty() {
        return write_error_response(
            socket,
            "INVALID_WP_CLIENT_TOKEN",
            "wp_client_token is required for new bindings or when changing to a different site",
        )
        .await;
    }
    let final_route_secret = if route_secret.is_empty() {
        existing_entry
            .map(|entry| entry.route_secret.clone())
            .unwrap_or_default()
    } else {
        route_secret
    };
    if !existing_domain_key.is_empty() && existing_domain_key != domain_key {
        bindings_doc.domains.remove(&existing_domain_key);
    }
    bindings_doc.domains.insert(
        domain_key.clone(),
        DomainTokenBindingEntry {
            wp_client_token: token.clone(),
            route_secret: final_route_secret.clone(),
        },
    );
    save_domain_token_bindings_runtime_doc(&path, &bindings_doc)?;
    {
        let mut guard = state.lock().await;
        {
            let db = guard.db.lock().await;
            let _ = crate::db::bindings::save_domain_token_bindings_doc(&db, &bindings_doc);
        }
        guard.domain_token_bindings = bindings_doc.clone();
        guard.domains = local_sites_from_domain_token_bindings(&bindings_doc);
        guard.last_error.clear();
        guard.last_event = "domain_tokens.upserted".to_string();
        guard.updated_at = unix_ts();
    }
    let payload = json!({
        "success": true,
        "data": {
            "api_base_url": domain_key,
            "token_prefix": session_token_prefix(&token),
            "token_len": token.len(),
            "route_secret_set": !final_route_secret.is_empty()
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_domain_tokens_delete(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiDeleteDomainTokenRequest = serde_json::from_slice(body)
        .with_context(|| "invalid /api/domain-tokens/delete json payload")?;
    let domain_key = normalize_domain_base(&req.api_base_url.unwrap_or_default());
    if domain_key.is_empty() {
        return write_error_response(socket, "INVALID_API_BASE_URL", "api_base_url is required")
            .await;
    }
    let (path, mut bindings_doc) = {
        let guard = state.lock().await;
        (
            guard.domain_token_bindings_path.clone(),
            guard.domain_token_bindings.clone(),
        )
    };
    let removed = bindings_doc.domains.remove(&domain_key).is_some();
    save_domain_token_bindings_runtime_doc(&path, &bindings_doc)?;
    {
        let mut guard = state.lock().await;
        {
            let db = guard.db.lock().await;
            let _ = crate::db::bindings::save_domain_token_bindings_doc(&db, &bindings_doc);
        }
        guard.domain_token_bindings = bindings_doc.clone();
        guard.domains = local_sites_from_domain_token_bindings(&bindings_doc);
        guard.last_error.clear();
        guard.last_event = "domain_tokens.deleted".to_string();
        guard.updated_at = unix_ts();
    }
    let payload = json!({
        "success": true,
        "data": {
            "api_base_url": domain_key,
            "deleted": removed
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_domain_tokens_test(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid /api/domain-tokens/test json payload")?;
    let domain_key = normalize_domain_base(req["api_base_url"].as_str().unwrap_or("").trim());
    if domain_key.is_empty() {
        return write_error_response(socket, "INVALID_API_BASE_URL", "api_base_url is required")
            .await;
    }

    let (token, domain_token_bindings, client, device_id) = {
        let guard = state.lock().await;
        let fallback = crate::config::env_or("WPTSALL_WP_CLIENT_TOKEN", "");
        match crate::bindings::resolve_wp_client_token_for_domain(
            &domain_key,
            &guard.domain_token_bindings,
            &fallback,
        ) {
            Some(token) => (
                token,
                guard.domain_token_bindings.clone(),
                guard.http_client.clone(),
                guard.device_id.clone(),
            ),
            None => {
                return write_error_response(
                    socket,
                    "NO_TOKEN_CONFIGURED",
                    "No WP client token configured for this domain. Add a domain token binding first.",
                )
                .await;
            }
        }
    };

    if token.is_empty() {
        return write_error_response(
            socket,
            "EMPTY_TOKEN",
            "WP client token is empty for this domain",
        )
        .await;
    }

    let route_secret =
        match resolve_effective_route_secret_for_domain(state, &domain_token_bindings, &domain_key)
            .await
        {
            Ok(route_secret) => route_secret,
            Err(err) => {
                if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                    return response;
                }
                return write_error_response(
                    socket,
                    "ROUTE_SECRET_RESOLUTION_FAILED",
                    &format!("{:#}", err),
                )
                .await;
            }
        };

    let wp_base = match route_secret
        .as_deref()
        .and_then(|s| build_wp_base_url(&domain_key, s))
    {
        Some(url) => url,
        None => {
            return write_error_response(
                socket,
                "MISSING_ROUTE_SECRET",
                "route_secret is missing for this domain. Configure it in Sites, or make sure server domains are refreshed and include route_secret.",
            )
            .await;
        }
    };

    let validate_url = format!("{}/validate-token", wp_base);
    let validate_route_missing = |err: &anyhow::Error| {
        matches!(
            find_passthrough_http_error(err),
            Some((_, code, _)) if code == "rest_no_route"
        ) || format!("{:#}", err).contains("rest_no_route")
    };

    let worker_id = if device_id.trim().is_empty() {
        "web-ui-test".to_string()
    } else {
        device_id
    };

    match wp_request_with_transport(
        &client,
        reqwest::Method::GET,
        &validate_url,
        &token,
        worker_id.as_str(),
        &json!({}),
        route_secret.as_deref(),
    )
    .await
    {
        Ok(resp_value) => {
            let payload = json!({
                "success": true,
                "data": resp_value
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(validate_err) => {
            let ping_err = if validate_route_missing(&validate_err) {
                let ping_url = format!("{}/ping", wp_base);
                match wp_request_with_transport(
                    &client,
                    reqwest::Method::GET,
                    &ping_url,
                    &token,
                    "web-ui-test",
                    &json!({}),
                    route_secret.as_deref(),
                )
                .await
                {
                    Ok(ping_value) => {
                        let payload = json!({
                            "success": true,
                            "data": {
                                "validated_via": "ping",
                                "validate_token_error": format!("{}", validate_err),
                                "ping": ping_value
                            }
                        });
                        return write_http_response(
                            socket,
                            "200 OK",
                            "application/json",
                            &serde_json::to_vec(&payload)?,
                        )
                        .await;
                    }
                    Err(err) => Some(err),
                }
            } else {
                None
            };

            let relations_url = format!("{}/site-relations", wp_base);
            match wp_request_with_transport(
                &client,
                reqwest::Method::GET,
                &relations_url,
                &token,
                "web-ui-test",
                &json!({}),
                route_secret.as_deref(),
            )
            .await
            {
                Ok(relations_value) => {
                    let payload = json!({
                        "success": true,
                        "data": {
                            "validated_via": "site_relations",
                            "validate_token_error": format!("{}", validate_err),
                            "ping_error": ping_err.as_ref().map(|err| format!("{}", err)),
                            "site_relations": relations_value
                        }
                    });
                    return write_http_response(
                        socket,
                        "200 OK",
                        "application/json",
                        &serde_json::to_vec(&payload)?,
                    )
                    .await;
                }
                Err(relations_err) => {
                    if !validate_route_missing(&validate_err) {
                        if let Some(response) =
                            maybe_write_upstream_api_error(socket, &validate_err).await
                        {
                            return response;
                        }
                    }
                    if let Some(err) = ping_err.as_ref() {
                        if let Some(response) = maybe_write_upstream_api_error(socket, err).await {
                            return response;
                        }
                    }
                    if let Some(response) =
                        maybe_write_upstream_api_error(socket, &relations_err).await
                    {
                        return response;
                    }
                    if !validate_route_missing(&validate_err) {
                        if let Some(response) =
                            maybe_write_upstream_api_error(socket, &validate_err).await
                        {
                            return response;
                        }
                    }
                    let payload = json!({
                        "success": false,
                        "error": {
                            "code": "CONNECTION_FAILED",
                            "message": format!(
                                "Failed to connect to WP site: validate-token error: {}; ping error: {}; site-relations error: {}",
                                validate_err,
                                ping_err
                                    .as_ref()
                                    .map(|err| err.to_string())
                                    .unwrap_or_else(|| "not attempted".to_string()),
                                relations_err
                            )
                        }
                    });
                    return write_http_response(
                        socket,
                        "200 OK",
                        "application/json",
                        &serde_json::to_vec(&payload)?,
                    )
                    .await;
                }
            }
        }
    }
}

pub(super) async fn handle_site_connections_import(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid /api/site-connections/import json payload")?;
    let pack = req
        .get("site_connection_pack")
        .or_else(|| req.get("connection_pack"))
        .or_else(|| req.get("pack"))
        .unwrap_or(&req);
    let Some(pack_obj) = pack.as_object() else {
        return write_error_response(
            socket,
            "INVALID_SITE_CONNECTION_PACK",
            "site_connection_pack must be a JSON object",
        )
        .await;
    };

    let schema = pack_obj
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if "wptsall-site-connection.v1" != schema {
        return write_error_response(
            socket,
            "INVALID_SITE_CONNECTION_PACK",
            "site_connection_pack schema must be wptsall-site-connection.v1",
        )
        .await;
    }

    let site_url_raw = pack_obj
        .get("site_url")
        .or_else(|| pack_obj.get("wp_client_base"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if site_url_raw.is_empty() {
        return write_error_response(
            socket,
            "INVALID_SITE_URL",
            "site_url or wp_client_base is required",
        )
        .await;
    }
    let site_url = normalize_domain_base(&site_url_raw);
    if site_url.is_empty() {
        return write_error_response(socket, "INVALID_SITE_URL", "site_url must be a valid URL")
            .await;
    }

    let wp_client_base = pack_obj
        .get("wp_client_base")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut route_secret = pack_obj
        .get("route_secret")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if route_secret.is_empty() {
        route_secret = extract_route_secret_from_client_base(&wp_client_base).unwrap_or_default();
    }

    let pairing_code = req
        .get("pairing_code")
        .or_else(|| pack_obj.get("pairing_code"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if pairing_code.is_empty() {
        return write_error_response(socket, "INVALID_PAIRING_CODE", "pairing_code is required")
            .await;
    }

    let pack_device_id = pack_obj
        .get("device_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let device_label = req
        .get("device_label")
        .or_else(|| pack_obj.get("device_label"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let client_device_id = {
        let guard = state.lock().await;
        guard.device_id.clone()
    };
    if !pack_device_id.is_empty() && pack_device_id != client_device_id {
        return write_error_response_with_status(
            socket,
            "409 Conflict",
            "PAIRING_DEVICE_MISMATCH",
            "Connection pack device_id does not match this client device.",
        )
        .await;
    }

    let claim_url = if !wp_client_base.is_empty() {
        format!("{}/pairing/claim", wp_client_base.trim_end_matches('/'))
    } else if !route_secret.is_empty() {
        format!(
            "{}/wp-json/wptsall/v2/{}/client/pairing/claim",
            site_url.trim_end_matches('/'),
            route_secret
        )
    } else {
        return write_error_response(
            socket,
            "INVALID_ROUTE_SECRET",
            "route_secret or wp_client_base is required",
        )
        .await;
    };

    let client = {
        let guard = state.lock().await;
        guard.http_client.clone()
    };
    let claim_payload = json!({
        "schema": "wptsall-pairing-claim.v1",
        "device_id": client_device_id,
        "pairing_code": pairing_code,
        "device_label": device_label,
    });
    let response = client
        .post(&claim_url)
        .json(&claim_payload)
        .send()
        .await
        .with_context(|| format!("pairing claim request failed: {}", claim_url))?;
    let status = response.status();
    let claim_text = response
        .text()
        .await
        .with_context(|| format!("pairing claim response read failed: {}", claim_url))?;
    if !status.is_success() {
        let status_line = format!(
            "{} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Error")
        );
        if let Ok(payload) = serde_json::from_str::<Value>(&claim_text) {
            let code = payload
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("PAIRING_CLAIM_FAILED");
            let message = payload
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .or_else(|| payload.get("message").and_then(Value::as_str))
                .unwrap_or(&claim_text)
                .to_string();
            return write_error_response_with_status(socket, &status_line, code, &message).await;
        }
        return write_error_response_with_status(
            socket,
            &status_line,
            "PAIRING_CLAIM_FAILED",
            &claim_text,
        )
        .await;
    }

    let claim_json: Value = serde_json::from_str(&claim_text)
        .with_context(|| format!("invalid pairing claim json response from {}", claim_url))?;
    let data = claim_json
        .get("data")
        .or_else(|| claim_json.get("result"))
        .unwrap_or(&claim_json);
    let token = data
        .get("client_token")
        .or_else(|| data.get("token"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if token.is_empty() {
        return write_error_response(
            socket,
            "INVALID_CLIENT_TOKEN",
            "pairing claim response did not include client_token",
        )
        .await;
    }
    if route_secret.is_empty() {
        route_secret = extract_route_secret_from_client_base(&wp_client_base).unwrap_or_default();
    }

    let domain_key = normalize_domain_base(&site_url);
    let (path, mut bindings_doc) = {
        let guard = state.lock().await;
        (
            guard.domain_token_bindings_path.clone(),
            guard.domain_token_bindings.clone(),
        )
    };
    bindings_doc.domains.insert(
        domain_key.clone(),
        DomainTokenBindingEntry {
            wp_client_token: token.clone(),
            route_secret: route_secret.clone(),
        },
    );
    save_domain_token_bindings_runtime_doc(&path, &bindings_doc)?;
    {
        let mut guard = state.lock().await;
        {
            let db = guard.db.lock().await;
            let _ = crate::db::bindings::save_domain_token_bindings_doc(&db, &bindings_doc);
        }
        guard.domain_token_bindings = bindings_doc.clone();
        guard.domains = local_sites_from_domain_token_bindings(&bindings_doc);
        guard.last_error.clear();
        guard.last_event = "site_connections.imported".to_string();
        guard.updated_at = unix_ts();
    }

    let payload = json!({
        "success": true,
        "data": {
            "api_base_url": domain_key,
            "token_prefix": session_token_prefix(&token),
            "token_len": token.len(),
            "route_secret_set": !route_secret.is_empty(),
            "pairing_claimed": true,
            "expires_at": data.get("expires_at").and_then(Value::as_i64).unwrap_or(0),
            "scopes": data.get("scopes").cloned().unwrap_or_else(|| json!([]))
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

fn extract_route_secret_from_client_base(input: &str) -> Option<String> {
    let trimmed = input.trim().trim_end_matches('/');
    let marker = "/wp-json/wptsall/v2/";
    let index = trimmed.find(marker)?;
    let rest = &trimmed[index + marker.len()..];
    let secret = rest.split('/').next().unwrap_or_default().trim();
    if secret.is_empty() {
        None
    } else {
        Some(secret.to_string())
    }
}
