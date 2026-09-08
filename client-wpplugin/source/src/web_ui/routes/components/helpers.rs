use super::*;

// ---------------------------------------------------------------------------
// Dynamic route dispatcher for new CRUD endpoints with path params
// ---------------------------------------------------------------------------

pub(crate) fn load_local_components_runtime_doc() -> ComponentsLocalDoc {
    if web_ui_sqlite_storage_enabled() {
        crate::db::components::load_runtime_local_components_doc()
    } else {
        load_components_local(&components_local_path()).unwrap_or_default()
    }
}

pub(crate) fn save_local_components_runtime_doc(doc: &ComponentsLocalDoc) -> anyhow::Result<()> {
    if web_ui_sqlite_storage_enabled() {
        crate::db::components::save_runtime_local_components_doc(doc)
    } else {
        save_components_local(&components_local_path(), doc)
    }
}

pub(crate) fn save_component_bindings_runtime_doc(
    path: &str,
    doc: &ComponentBindingsDoc,
) -> anyhow::Result<()> {
    if web_ui_sqlite_storage_enabled() {
        crate::db::bindings::save_runtime_component_bindings_doc(doc)
    } else {
        save_component_bindings(path, doc)
    }
}

pub(crate) fn save_domain_token_bindings_runtime_doc(
    path: &str,
    doc: &DomainTokenBindingsDoc,
) -> anyhow::Result<()> {
    if web_ui_sqlite_storage_enabled() {
        crate::db::bindings::save_runtime_domain_token_bindings_doc(doc)
    } else {
        save_domain_token_bindings(path, doc)
    }
}

pub(crate) fn save_task_type_component_bindings_runtime_doc(
    path: &str,
    doc: &TaskTypeComponentBindingsDoc,
) -> anyhow::Result<()> {
    if web_ui_sqlite_storage_enabled() {
        crate::db::bindings::save_runtime_task_type_component_bindings_doc(doc)
    } else {
        save_task_type_component_bindings(path, doc)
    }
}

pub(crate) fn save_rule_component_bindings_runtime_doc(
    path: &str,
    doc: &RuleComponentBindingsDoc,
) -> anyhow::Result<()> {
    if web_ui_sqlite_storage_enabled() {
        crate::db::bindings::save_runtime_rule_component_bindings_doc(doc)
    } else {
        save_rule_component_bindings(path, doc)
    }
}

pub(crate) fn component_exists_in_local_doc(doc: &ComponentsLocalDoc, component_id: &str) -> bool {
    let normalized_id = component_id.trim();
    if normalized_id.is_empty() {
        return false;
    }
    doc.components.contains_key(normalized_id)
}

pub(crate) fn local_component_capability_id(kind: &str) -> String {
    match crate::component_rt::loader::normalize_component_runtime_kind(kind).as_deref() {
        Some("openai_compatible") => "text".to_string(),
        Some(normalized) => normalized.to_string(),
        None => kind.trim().to_lowercase(),
    }
}

pub(crate) fn find_server_component_by_template_id<'a>(
    components: &'a [ComponentItem],
    template_id: &str,
) -> Option<&'a ComponentItem> {
    let normalized_id = template_id.trim();
    if normalized_id.is_empty() {
        return None;
    }
    components
        .iter()
        .find(|component| component.id == normalized_id)
}

async fn fetch_server_component_for_session(
    client: &reqwest::Client,
    server_base: &str,
    session_token: &str,
    template_id: &str,
) -> anyhow::Result<ComponentItem> {
    let mut headers = HeaderMap::new();
    headers.insert("X-Client-Session", session_token.parse()?);
    let url = format!("{}/api/v1/components/{}", server_base, template_id.trim());
    let response_raw: Value = request_json_encrypted(
        client.get(url).headers(headers),
        "component detail",
        session_token,
    )
    .await?;

    if let Ok(resp) =
        serde_json::from_value::<ApiResponse<ComponentManageData>>(response_raw.clone())
    {
        if resp.success {
            return Ok(resp.data.component);
        }
    }
    if let Ok(data) = serde_json::from_value::<ComponentManageData>(response_raw.clone()) {
        return Ok(data.component);
    }

    anyhow::bail!(
        "component detail: unsupported response shape ({})",
        response_raw
    );
}

fn component_api_version_unsupported_error(
    template_id: &str,
    api_version: Option<&str>,
) -> UpstreamApiError {
    let rendered_api_version = api_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    UpstreamApiError {
        status: "422 Unprocessable Entity".to_string(),
        code: "COMPONENT_API_VERSION_UNSUPPORTED".to_string(),
        message: format!(
            "template '{}' requires unsupported api_version '{}'",
            template_id.trim(),
            rendered_api_version
        ),
    }
}

pub(crate) fn validate_server_component_api_version(
    template_id: &str,
    server_component: &ComponentItem,
) -> anyhow::Result<()> {
    if crate::component_rt::loader::is_component_api_version_compatible(
        server_component.api_version.as_deref(),
    ) {
        return Ok(());
    }
    Err(component_api_version_unsupported_error(
        template_id,
        server_component.api_version.as_deref(),
    )
    .into())
}

fn effective_local_component_api_version(
    comp: &crate::types::ComponentInstanceLocal,
    server_components: &[ComponentItem],
) -> Option<String> {
    find_server_component_by_template_id(server_components, &comp.template_id)
        .and_then(|component| component.api_version.clone())
        .or_else(|| comp.source_template_api_version.clone())
}

pub(crate) fn validate_local_component_api_version_with_cached_components(
    comp: &crate::types::ComponentInstanceLocal,
    server_components: &[ComponentItem],
) -> anyhow::Result<()> {
    let effective_api_version = effective_local_component_api_version(comp, server_components);
    if crate::component_rt::loader::is_component_api_version_compatible(
        effective_api_version.as_deref(),
    ) {
        return Ok(());
    }
    Err(component_api_version_unsupported_error(
        &comp.template_id,
        effective_api_version.as_deref(),
    )
    .into())
}

pub(crate) fn normalized_vendor_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn validate_component_binding_vendor_alignment(
    component_id: &str,
    component_vendor_id: Option<&str>,
    key_ids: &[String],
    oauth_ids: &[String],
    vendor_keys_doc: &VendorKeysDoc,
    vendor_oauth_doc: &VendorOAuthDoc,
) -> anyhow::Result<()> {
    if key_ids.is_empty() && oauth_ids.is_empty() {
        return Ok(());
    }

    let Some(expected_vendor_id) = component_vendor_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        anyhow::bail!(
            "component '{}' has no vendor_id; bind a vendor to the component before attaching key/oauth pools",
            component_id
        );
    };

    let mut mismatched: Vec<String> = Vec::new();
    for key_id in key_ids {
        let Some(vendor_key) = vendor_keys_doc.keys.get(key_id) else {
            anyhow::bail!("vendor key '{}' not found", key_id);
        };
        if vendor_key.vendor_id.trim() != expected_vendor_id {
            mismatched.push(format!("key:{}=>{}", key_id, vendor_key.vendor_id.trim()));
        }
    }
    for oauth_id in oauth_ids {
        let Some(oauth_config) = vendor_oauth_doc.configs.get(oauth_id) else {
            anyhow::bail!("oauth config '{}' not found", oauth_id);
        };
        if oauth_config.vendor_id.trim() != expected_vendor_id {
            mismatched.push(format!(
                "oauth:{}=>{}",
                oauth_id,
                oauth_config.vendor_id.trim()
            ));
        }
    }

    if mismatched.is_empty() {
        return Ok(());
    }

    anyhow::bail!(
        "component '{}' expects vendor '{}' but received mismatched auth resources: {}",
        component_id,
        expected_vendor_id,
        mismatched.join(", ")
    );
}

pub(crate) fn patch_template_snapshot_for_local_kind(template_json: &mut Value, local_kind: &str) {
    let Some(template_type) =
        crate::component_rt::loader::canonical_template_type_for_local_kind(local_kind)
    else {
        return;
    };
    let supported_type = crate::component_rt::loader::normalize_component_runtime_kind(local_kind)
        .filter(|kind| kind != "openai_compatible")
        .unwrap_or_else(|| "text".to_string());
    let Some(obj) = template_json.as_object_mut() else {
        return;
    };
    obj.insert("type".to_string(), Value::String(template_type.to_string()));
    obj.insert(
        "supported_types".to_string(),
        Value::Array(vec![Value::String(supported_type)]),
    );
}

pub(crate) async fn fetch_server_template_snapshot_for_local_component(
    state: &Arc<Mutex<WebUiState>>,
    template_id: &str,
    local_kind: &str,
) -> anyhow::Result<(Value, Option<ComponentItem>)> {
    let (server_base, session_token_opt, client, mut server_component) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
            find_server_component_by_template_id(&guard.components, template_id).cloned(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return Err(anyhow!("session required — please log in first"));
    };

    if server_component.is_none() {
        server_component = Some(
            fetch_server_component_for_session(&client, &server_base, &session_token, template_id)
                .await?,
        );
        let mut guard = state.lock().await;
        guard
            .components
            .retain(|component| component.id != template_id);
        if let Some(component) = server_component.clone() {
            guard.components.push(component);
            guard
                .components
                .sort_by(|left, right| left.id.cmp(&right.id));
        }
        guard.updated_at = unix_ts();
    }

    if let Some(ref server_component) = server_component {
        validate_server_component_api_version(template_id, server_component)?;
        if let Some(normalized_kind) =
            crate::component_rt::loader::normalize_component_runtime_kind(local_kind)
        {
            if !crate::component_rt::loader::component_kind_matches_supported_types(
                &normalized_kind,
                &server_component.supported_types,
            ) {
                return Err(anyhow!(
                    "template '{}' does not support local component kind '{}'",
                    template_id,
                    local_kind
                ));
            }
        }
    }

    let mut headers = HeaderMap::new();
    headers.insert("X-Client-Session", session_token.parse()?);
    let download_url = format!(
        "{}/api/v1/client/components/{}/download",
        server_base, template_id
    );
    let download_resp = request_component_download_with_passthrough(
        client.get(download_url).headers(headers),
        "client component download (local snapshot)",
    )
    .await?;
    let mut template_json = resolve_download_template_with_signing_key(
        state,
        &client,
        &server_base,
        &session_token,
        &download_resp.data,
        "client component download (local snapshot)",
    )
    .await?;
    patch_template_snapshot_for_local_kind(&mut template_json, local_kind);
    Ok((template_json, server_component))
}

pub(crate) async fn ensure_local_component_snapshot(
    state: &Arc<Mutex<WebUiState>>,
    component_id: &str,
) -> anyhow::Result<(crate::types::ComponentInstanceLocal, Value)> {
    // P0-LF-03 5.5: in local mode the inline template and the local record's
    // API version are the ONLY sources; the server component cache and the
    // website are never consulted.
    let local_mode = !crate::config::server_control_plane_enabled();
    let cached_server_components = if local_mode {
        Vec::new()
    } else {
        let guard = state.lock().await;
        guard.components.clone()
    };
    let mut doc = load_local_components_runtime_doc();
    let comp = doc
        .components
        .get_mut(component_id)
        .ok_or_else(|| anyhow!("local component '{}' not found", component_id))?;

    validate_local_component_api_version_with_cached_components(comp, &cached_server_components)?;

    if let Some(snapshot) = comp.template_json.clone() {
        return Ok((comp.clone(), snapshot));
    }

    if comp.kind.eq_ignore_ascii_case("openai_compatible") {
        return Err(anyhow!(
            "local component '{}' has no inline template snapshot",
            component_id
        ));
    }

    let template_id = comp.template_id.trim().to_string();
    if template_id.is_empty() {
        return Err(anyhow!(
            "local component '{}' has no template snapshot or template_id",
            component_id
        ));
    }

    // P0-LF-03 5.5: no implicit server snapshot download in local mode; the
    // component must carry an inline template (or be installed from the
    // signed local catalog).
    if local_mode {
        return Err(anyhow!(
            "local component '{}' has no inline template snapshot; install it from the local catalog or provide template_json",
            component_id
        ));
    }

    let (snapshot, server_component) =
        fetch_server_template_snapshot_for_local_component(state, &template_id, &comp.kind).await?;
    comp.template_json = Some(snapshot.clone());
    if comp.source_template_id.trim().is_empty() {
        comp.source_template_id = template_id.clone();
    }
    if comp.source_template_updated_at.is_none() {
        comp.source_template_updated_at = server_component
            .as_ref()
            .and_then(|component| component.updated_at.clone());
    }
    comp.source_template_api_version = server_component
        .as_ref()
        .and_then(|component| component.api_version.clone());
    if comp.vendor_id.trim().is_empty() {
        if let Some(server_vendor_id) = server_component
            .as_ref()
            .and_then(|component| component.vendor_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            comp.vendor_id = server_vendor_id.to_string();
        }
    }
    comp.updated_at = Some(format!("{}", unix_ts()));
    let updated = comp.clone();
    save_local_components_runtime_doc(&doc)?;
    Ok((updated, snapshot))
}

pub(crate) fn backfill_local_components_from_server(
    doc: &mut ComponentsLocalDoc,
    server_components: &[ComponentItem],
) -> usize {
    if doc.components.is_empty() || server_components.is_empty() {
        return 0;
    }

    let mut updated = 0usize;
    for component in doc.components.values_mut() {
        if component.kind.eq_ignore_ascii_case("openai_compatible") {
            let had_legacy_template_refs = !component.template_id.trim().is_empty()
                || !component.source_template_id.trim().is_empty()
                || component.source_template_updated_at.is_some()
                || component.source_template_api_version.is_some();
            if had_legacy_template_refs {
                component.template_id.clear();
                component.source_template_id.clear();
                component.source_template_updated_at = None;
                component.source_template_api_version = None;
                updated += 1;
            }
            continue;
        }

        let template_id = component.template_id.trim();
        if template_id.is_empty() {
            continue;
        }

        let Some(server_component) =
            find_server_component_by_template_id(server_components, template_id)
        else {
            continue;
        };

        let mut changed = false;
        if component.vendor_id.trim().is_empty() {
            if let Some(server_vendor_id) = server_component
                .vendor_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                component.vendor_id = server_vendor_id.to_string();
                changed = true;
            }
        }

        if component.source_template_api_version != server_component.api_version {
            component.source_template_api_version = server_component.api_version.clone();
            changed = true;
        }

        if component.template_json.is_none()
            && !component.kind.eq_ignore_ascii_case("openai_compatible")
        {
            if let Some(server_kind) = crate::component_rt::loader::normalize_component_runtime_kind(
                &server_component.kind,
            ) {
                if !component.kind.eq_ignore_ascii_case(&server_kind) {
                    component.kind = server_kind;
                    changed = true;
                }
            }
        }

        if changed {
            updated += 1;
        }
    }

    updated
}

pub(crate) fn sync_local_components_from_server(
    server_components: &[ComponentItem],
) -> anyhow::Result<usize> {
    if server_components.is_empty() {
        return Ok(0);
    }

    let mut doc = load_local_components_runtime_doc();
    let updated = backfill_local_components_from_server(&mut doc, server_components);
    if updated > 0 {
        save_local_components_runtime_doc(&doc)?;
    }
    Ok(updated)
}

pub(crate) async fn refresh_local_component_snapshot_from_server(
    state: &Arc<Mutex<WebUiState>>,
    component_id: &str,
) -> anyhow::Result<crate::types::ComponentInstanceLocal> {
    let mut doc = load_local_components_runtime_doc();
    let comp = doc
        .components
        .get_mut(component_id)
        .ok_or_else(|| anyhow!("component not found"))?;

    if comp.kind.eq_ignore_ascii_case("openai_compatible") {
        anyhow::bail!("openai_compatible components do not use server snapshots");
    }

    let template_id = comp.template_id.trim().to_string();
    if template_id.is_empty() {
        anyhow::bail!("component has no template_id");
    }

    let (snapshot, server_component) =
        fetch_server_template_snapshot_for_local_component(state, &template_id, &comp.kind).await?;
    comp.template_json = Some(snapshot);
    comp.source_template_id = template_id;
    comp.source_template_updated_at = server_component
        .as_ref()
        .and_then(|component| component.updated_at.clone());
    comp.source_template_api_version = server_component
        .as_ref()
        .and_then(|component| component.api_version.clone());
    if let Some(server_vendor_id) = server_component
        .as_ref()
        .and_then(|component| component.vendor_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        comp.vendor_id = server_vendor_id.to_string();
    }
    comp.updated_at = Some(format!("{}", unix_ts()));
    let updated = comp.clone();
    save_local_components_runtime_doc(&doc)?;
    Ok(updated)
}

#[derive(Debug, Default)]
pub(crate) struct ComponentUsageSummary {
    pub(crate) references: Vec<String>,
    pub(crate) relation_ids: Vec<String>,
}

pub(crate) fn collect_component_usage_summary(
    component_id: &str,
    task_type_bindings: &TaskTypeComponentBindingsDoc,
    rule_bindings: &RuleComponentBindingsDoc,
) -> ComponentUsageSummary {
    let mut summary = ComponentUsageSummary::default();

    for (task_type, entry) in &task_type_bindings.task_types {
        if entry.component_id == component_id {
            summary.references.push(format!("task_type:{}", task_type));
        }
    }
    for (scoped_key, entry) in &task_type_bindings.business_line_task_types {
        if entry.component_id != component_id {
            continue;
        }
        let label = if let Some(parsed) = parse_business_line_task_type_binding_key(scoped_key) {
            parsed.replace(':', "::")
        } else {
            scoped_key.clone()
        };
        summary.references.push(format!("task_type:{}", label));
    }

    for (slot_key, cid) in &rule_bindings.global_defaults {
        if cid == component_id {
            summary.references.push(format!("global:{}", slot_key));
        }
    }
    for (plugin_slug, slot_map) in &rule_bindings.plugin_bindings {
        for (slot_key, cid) in slot_map {
            if cid == component_id {
                summary
                    .references
                    .push(format!("plugin:{}:{}", plugin_slug, slot_key));
            }
        }
    }
    for (relation_id, slot_map) in &rule_bindings.relation_bindings {
        for (slot_key, cid) in slot_map {
            if cid == component_id {
                summary
                    .references
                    .push(format!("relation:{}:{}", relation_id, slot_key));
                summary.relation_ids.push(relation_id.clone());
            }
        }
    }
    for (rule_id, slot_map) in &rule_bindings.rule_bindings {
        for (slot_key, cid) in slot_map {
            if cid == component_id {
                summary
                    .references
                    .push(format!("rule:{}:{}", rule_id, slot_key));
            }
        }
    }

    summary.references.sort();
    summary.references.dedup();
    summary.relation_ids.sort();
    summary.relation_ids.dedup();
    summary
}

pub(crate) fn component_in_use_message(summary: &ComponentUsageSummary) -> String {
    if !summary.relation_ids.is_empty() {
        return format!(
            "当前组件已被站点关系使用（relation_id: {}），请手动移除后再删除或者停用。",
            summary.relation_ids.join(", ")
        );
    }
    format!(
        "当前组件已被绑定使用（{}），请手动移除后再删除或者停用。",
        summary.references.join("；")
    )
}

fn task_editable_overrides_to_component_overrides(
    value: Option<&serde_json::Value>,
) -> ComponentInstanceOverrides {
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return ComponentInstanceOverrides::default();
    };

    let mut constraints_override = ComponentConstraints::default();
    let mut has_constraints = false;
    let mut request_headers: HashMap<String, String> = HashMap::new();
    let mut request_body_map = serde_json::Map::new();
    let mut request_url: Option<String> = None;
    let mut default_values_map = serde_json::Map::new();

    for (path, raw_value) in obj {
        let normalized = path.trim();
        if normalized == "request.url" {
            if let Some(v) = raw_value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
                request_url = Some(v.to_string());
            }
            continue;
        }
        if let Some(key) = normalized.strip_prefix("request.headers.") {
            if let Some(v) = raw_value.as_str() {
                request_headers.insert(key.to_string(), v.to_string());
            }
            continue;
        }
        if let Some(key) = normalized.strip_prefix("request.body.") {
            request_body_map.insert(key.to_string(), raw_value.clone());
            continue;
        }
        if let Some(key) = normalized.strip_prefix("default_values.") {
            default_values_map.insert(key.to_string(), raw_value.clone());
            continue;
        }
        match normalized {
            "constraints.max_input_chars" => {
                constraints_override.max_input_chars = raw_value.as_u64();
                has_constraints = true;
            }
            "constraints.max_input_bytes" => {
                constraints_override.max_input_bytes = raw_value.as_u64();
                has_constraints = true;
            }
            "constraints.rate_limit_rpm" => {
                constraints_override.rate_limit_rpm = raw_value.as_u64().map(|v| v as u32);
                has_constraints = true;
            }
            "constraints.rate_limit_qps" => {
                constraints_override.rate_limit_qps = raw_value.as_u64().map(|v| v as u32);
                has_constraints = true;
            }
            "constraints.max_concurrent_requests" => {
                constraints_override.max_concurrent_requests = raw_value.as_u64().map(|v| v as u32);
                has_constraints = true;
            }
            "constraints.max_file_size_mb" => {
                constraints_override.max_file_size_mb = raw_value.as_f64().map(|v| v as u32);
                has_constraints = true;
            }
            "constraints.split_strategy" => {
                constraints_override.split_strategy = raw_value.as_str().map(|v| v.to_string());
                has_constraints = true;
            }
            "constraints.split_separator" => {
                constraints_override.split_separator = raw_value.as_str().map(|v| v.to_string());
                has_constraints = true;
            }
            _ => {}
        }
    }

    ComponentInstanceOverrides {
        constraints_override: if has_constraints {
            Some(constraints_override)
        } else {
            None
        },
        request_overrides: if request_url.is_some()
            || !request_headers.is_empty()
            || !request_body_map.is_empty()
        {
            Some(ComponentRequestOverrides {
                method: None,
                url: request_url,
                headers: if request_headers.is_empty() {
                    None
                } else {
                    Some(request_headers)
                },
                body: if request_body_map.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(request_body_map))
                },
            })
        } else {
            None
        },
        default_values_override: if default_values_map.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(default_values_map))
        },
    }
}

fn resolve_task_override_component_id(
    item: &crate::db::jobs::TranslationItem,
    selected_component_id: Option<&str>,
) -> Option<String> {
    selected_component_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            item.selected_component_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            let current = item.component_id.trim();
            if current.is_empty() {
                None
            } else {
                Some(current.to_string())
            }
        })
}

pub(crate) fn invalid_task_override_paths(
    template: &ComponentTemplate,
    editable_overrides: &serde_json::Value,
) -> Vec<String> {
    let Some(obj) = editable_overrides.as_object() else {
        return vec![];
    };

    let mut invalid_paths: Vec<String> = obj
        .keys()
        .filter_map(|path| {
            let normalized = path.trim();
            if normalized.is_empty() {
                return Some(path.clone());
            }
            if crate::component_rt::loader::template_allows_editable_path(template, normalized) {
                None
            } else {
                Some(normalized.to_string())
            }
        })
        .collect();
    invalid_paths.sort();
    invalid_paths
}

pub(crate) fn validate_editable_overrides_for_component_id(
    component_id: Option<&str>,
    editable_overrides: Option<&serde_json::Value>,
) -> anyhow::Result<()> {
    let Some(editable_overrides) = editable_overrides else {
        return Ok(());
    };
    let Some(obj) = editable_overrides.as_object() else {
        return Err(anyhow!("editable_overrides must be a JSON object"));
    };
    if obj.is_empty() {
        return Ok(());
    }

    let component_id = component_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("editable_overrides requires a resolvable local component"))?;
    let local_doc = load_local_components_runtime_doc();
    let comp = local_doc
        .components
        .get(component_id)
        .ok_or_else(|| anyhow!("local component '{}' not found", component_id))?;
    let template: ComponentTemplate = comp
        .template_json
        .clone()
        .ok_or_else(|| {
            anyhow!(
                "local component '{}' has no template snapshot",
                component_id
            )
        })
        .and_then(|value| {
            serde_json::from_value(value).with_context(|| {
                format!(
                    "invalid local component template snapshot for '{}'",
                    component_id
                )
            })
        })?;

    let invalid_paths = invalid_task_override_paths(&template, editable_overrides);
    if invalid_paths.is_empty() {
        return Ok(());
    }

    Err(anyhow!(
        "editable_overrides contains paths outside editable_params: {}",
        invalid_paths.join(", ")
    ))
}

pub(crate) fn validate_task_editable_overrides(
    item: &crate::db::jobs::TranslationItem,
    selected_component_id: Option<&str>,
    editable_overrides: Option<&serde_json::Value>,
) -> anyhow::Result<()> {
    let component_id = resolve_task_override_component_id(item, selected_component_id);
    validate_editable_overrides_for_component_id(component_id.as_deref(), editable_overrides)
}

pub(crate) async fn validate_local_component_runtime_ready_for_task(
    state: &Arc<Mutex<WebUiState>>,
    component_id: &str,
    task_editable_overrides: Option<&serde_json::Value>,
) -> anyhow::Result<()> {
    let _ = build_local_component_runtime_for_task(state, component_id, task_editable_overrides)
        .await?;
    Ok(())
}

fn merge_component_instance_overrides(
    base: Option<&ComponentInstanceOverrides>,
    task: Option<&serde_json::Value>,
) -> Option<ComponentInstanceOverrides> {
    let task_overrides = task_editable_overrides_to_component_overrides(task);
    let mut merged = base.cloned().unwrap_or_default();
    if task_overrides.constraints_override.is_some() {
        merged.constraints_override = task_overrides.constraints_override;
    }
    if task_overrides.request_overrides.is_some() {
        merged.request_overrides = task_overrides.request_overrides;
    }
    if task_overrides.default_values_override.is_some() {
        merged.default_values_override = task_overrides.default_values_override;
    }
    if merged.constraints_override.is_none()
        && merged.request_overrides.is_none()
        && merged.default_values_override.is_none()
    {
        None
    } else {
        Some(merged)
    }
}

pub(crate) async fn build_local_component_runtime_for_task(
    state: &Arc<Mutex<WebUiState>>,
    component_id: &str,
    task_editable_overrides: Option<&serde_json::Value>,
) -> anyhow::Result<ComponentRuntime> {
    let component_id = component_id.trim();
    if component_id.is_empty() {
        return Err(anyhow!("component_id is required"));
    }

    let local_doc = load_local_components_runtime_doc();
    let comp = local_doc
        .components
        .get(component_id)
        .cloned()
        .ok_or_else(|| anyhow!("local component '{}' not found", component_id))?;
    if !comp.enabled {
        return Err(anyhow!("local component '{}' is disabled", component_id));
    }
    // P0-LF-03 5.3: version validation uses LOCAL data only in local mode;
    // the server component cache is never consulted.
    if crate::config::server_control_plane_enabled() {
        let cached_server_components = {
            let guard = state.lock().await;
            guard.components.clone()
        };
        validate_local_component_api_version_with_cached_components(
            &comp,
            &cached_server_components,
        )?;
    } else {
        validate_local_component_api_version_with_cached_components(&comp, &[])?;
    }
    let mut template: ComponentTemplate = comp
        .template_json
        .clone()
        .ok_or_else(|| {
            anyhow!(
                "local component '{}' has no template snapshot",
                component_id
            )
        })
        .and_then(|tpl| {
            serde_json::from_value(tpl)
                .with_context(|| format!("invalid local component template for '{}'", component_id))
        })?;
    crate::component_rt::loader::apply_local_component_kind_to_template(&mut template, &comp.kind);
    let merged_component_overrides = merge_component_instance_overrides(
        comp.component_overrides.as_ref(),
        task_editable_overrides,
    );
    crate::component_rt::loader::apply_component_instance_overrides_to_template(
        &mut template,
        merged_component_overrides.as_ref(),
    );

    let component_bindings = {
        let guard = state.lock().await;
        guard.component_bindings.clone()
    };
    let binding_entry = component_bindings.components.get(component_id);
    let has_pooled_auth = binding_entry
        .map(|entry| !entry.key_ids.is_empty() || !entry.oauth_ids.is_empty())
        .unwrap_or(false);
    let mut component_bindings_mut = component_bindings.clone();
    let (mut auth_values, _updated) = match crate::component_rt::runner::resolve_auth_values(
        component_id,
        template.auth.as_ref(),
        &mut component_bindings_mut,
    ) {
        Ok(result) => result,
        Err(_) if has_pooled_auth => (HashMap::new(), false),
        Err(err) => return Err(err),
    };
    crate::component_rt::loader::apply_binding_overrides_to_template(&mut template, binding_entry);

    if let Some(entry) = binding_entry {
        let vendor_keys = load_vendor_keys(&vendor_keys_path()).unwrap_or_default();
        let expected_vendor_id = normalized_vendor_id(&comp.vendor_id);
        for key_id in &entry.key_ids {
            if let Some(vendor_key) = vendor_keys.keys.get(key_id) {
                if vendor_key.enabled
                    && expected_vendor_id
                        .as_deref()
                        .map(|expected| vendor_key.vendor_id.trim() == expected)
                        .unwrap_or(true)
                {
                    for (k, v) in &vendor_key.auth_values {
                        auth_values.insert(format!("auth.{}", k), v.clone());
                    }
                    break;
                }
            }
        }
        if auth_values.is_empty() && !entry.oauth_ids.is_empty() {
            let oauth_doc = load_vendor_oauth(&vendor_oauth_path()).unwrap_or_default();
            if let Some(oauth_id) = entry.oauth_ids.iter().find(|oauth_id| {
                oauth_doc
                    .configs
                    .get(*oauth_id)
                    .map(|cfg| {
                        expected_vendor_id
                            .as_deref()
                            .map(|expected| cfg.vendor_id.trim() == expected)
                            .unwrap_or(true)
                    })
                    .unwrap_or(false)
            }) {
                if let Some(cfg) = oauth_doc.configs.get(oauth_id) {
                    if let Some(token) = cfg.cached_token.as_ref().filter(|t| !t.trim().is_empty())
                    {
                        auth_values.insert(format!("auth.{}", cfg.token_field), token.clone());
                    }
                }
            }
        }
    }

    let normalized_kind = crate::component_rt::loader::normalize_component_runtime_kind(&comp.kind)
        .or_else(|| crate::component_rt::loader::normalize_component_runtime_kind(&template.kind))
        .unwrap_or_else(|| "text".to_string());
    let supported_content_formats =
        crate::component_rt::loader::resolve_local_supported_content_formats(
            &normalized_kind,
            &template,
        );
    let supported_formats = template
        .constraints
        .as_ref()
        .and_then(|c| c.supported_formats.clone())
        .unwrap_or_default();
    let (
        runtime_max_concurrent_requests,
        runtime_min_interval_ms,
        runtime_concurrency_sem,
        runtime_last_request_at,
    ) = crate::component_rt::loader::runtime_limits_from_constraints(template.constraints.as_ref());

    Ok(ComponentRuntime {
        template,
        auth_values,
        supported_business_lines: vec![],
        language_map: binding_entry
            .map(|e| e.language_map.clone())
            .unwrap_or_default(),
        supported_content_formats,
        supported_formats,
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        proxy_profile_id: None,
        runtime_max_concurrent_requests,
        runtime_min_interval_ms,
        runtime_concurrency_sem,
        runtime_last_request_at,
    })
}
