use super::*;

// ---------------------------------------------------------------------------
// Dynamic route dispatcher for new CRUD endpoints with path params
// ---------------------------------------------------------------------------

pub(crate) fn load_local_components_runtime_doc() -> anyhow::Result<ComponentsLocalDoc> {
    if web_ui_sqlite_storage_enabled() {
        crate::db::components::load_runtime_local_components_doc()
    } else {
        load_components_local(&components_local_path()).map_err(|_| {
            crate::component_rt::loader::RuntimeConfigurationFault::error("local components file")
        })
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
    let mut doc = load_local_components_runtime_doc()?;
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

    let mut doc = load_local_components_runtime_doc()?;
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
    let mut doc = load_local_components_runtime_doc()?;
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
) -> anyhow::Result<ComponentInstanceOverrides> {
    crate::component_rt::loader::task_editable_overrides_to_component_overrides(value)
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
    let local_doc = load_local_components_runtime_doc()?;
    let comp = local_doc
        .components
        .get(component_id)
        .ok_or_else(|| anyhow!("local component '{}' not found", component_id))?;
    let mut template: ComponentTemplate = comp
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
        let overrides = task_editable_overrides_to_component_overrides(Some(editable_overrides))?;
        crate::component_rt::loader::apply_component_instance_overrides_to_template(
            &mut template,
            Some(&overrides),
        )?;
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

pub(crate) async fn build_local_component_runtime_for_task(
    state: &Arc<Mutex<WebUiState>>,
    component_id: &str,
    task_editable_overrides: Option<&serde_json::Value>,
) -> anyhow::Result<ComponentRuntime> {
    let component_id = component_id.trim();
    if component_id.is_empty() {
        return Err(anyhow!("component_id is required"));
    }

    let local_doc = load_local_components_runtime_doc()?;
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
    let vendor_keys = load_vendor_keys(&vendor_keys_path()).map_err(|_| {
        crate::component_rt::loader::RuntimeConfigurationFault::error("vendor keys")
    })?;
    let vendor_oauth = load_vendor_oauth(&vendor_oauth_path()).map_err(|_| {
        crate::component_rt::loader::RuntimeConfigurationFault::error("vendor OAuth")
    })?;
    let proxy_profiles = load_proxy_profiles(&proxy_profiles_path()).map_err(|_| {
        crate::component_rt::loader::RuntimeConfigurationFault::error("proxy profiles")
    })?;
    let proxy_pool = crate::component_rt::proxy::ProxyClientPool::new(&proxy_profiles.profiles)?;
    let selected_version = crate::component_rt::loader::select_local_component_version(&comp);
    let proxy_profile_id = selected_version
        .and_then(|(_, version)| version.proxy_profile_id.clone())
        .filter(|id| !id.trim().is_empty());
    // Validate the selected provider and token transports before source fetch
    // or paid intent. An absent proxy never falls back to direct egress.
    proxy_pool.get_client(proxy_profile_id.as_deref())?;
    let oauth_transport = proxy_pool
        .get_oauth_client(proxy_profile_id.as_deref())?
        .clone();
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
    template.id = component_id.to_string();
    crate::component_rt::loader::apply_component_instance_overrides_to_template(
        &mut template,
        comp.component_overrides.as_ref(),
    )?;
    if let Some((_, version)) = selected_version {
        crate::component_rt::loader::apply_version_config_overrides_to_template(
            &mut template,
            &version.config_overrides,
        )?;
    }

    let (component_bindings, db) = {
        let guard = state.lock().await;
        (guard.component_bindings.clone(), guard.db.clone())
    };
    let binding_entry = component_bindings.components.get(component_id);
    // Components created via the provider wizard keep their credentials in the
    // vendor-key store linked from a component VERSION (no legacy binding
    // entry); the selected version's key_ids also count as pooled auth.
    let has_version_keys = selected_version
        .map(|(_, version)| !version.key_ids.is_empty())
        .unwrap_or(false);
    let has_pooled_auth = binding_entry
        .map(|entry| !entry.key_ids.is_empty() || !entry.oauth_ids.is_empty())
        .unwrap_or(false)
        || has_version_keys;
    let pool_binding = crate::component_rt::loader::local_component_pool_binding(
        binding_entry,
        selected_version.map(|(_, version)| version),
    )?;
    let (key_pool, oauth_pool) = crate::component_rt::loader::build_component_pools(
        component_id,
        Some(&comp.vendor_id),
        pool_binding.as_ref(),
        &vendor_keys,
        &vendor_oauth,
    )?;
    anyhow::ensure!(
        !has_pooled_auth || key_pool.is_some() || oauth_pool.is_some(),
        "component '{}' has no usable auth in its configured credential pool",
        component_id
    );
    let mut component_bindings_mut = component_bindings.clone();
    let mut auth_resolve_failed = false;
    let (mut auth_values, _updated) = match crate::component_rt::runner::resolve_auth_values(
        component_id,
        template.auth.as_ref(),
        &mut component_bindings_mut,
    ) {
        Ok(result) => result,
        Err(_) if has_pooled_auth => {
            auth_resolve_failed = true;
            (HashMap::new(), false)
        }
        Err(err) => return Err(err),
    };
    crate::component_rt::loader::apply_binding_overrides_to_template(&mut template, binding_entry);
    let task_overrides = task_editable_overrides_to_component_overrides(task_editable_overrides)?;
    crate::component_rt::loader::apply_component_instance_overrides_to_template(
        &mut template,
        Some(&task_overrides),
    )?;

    if let Some(entry) = binding_entry {
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
    }

    // Wizard track: resolve credentials from the selected version's key_ids
    // via the vendor-key store, exactly like the version quick-test does.
    if auth_values.is_empty() {
        if let Some((_, version)) = selected_version {
            let expected_vendor_id = normalized_vendor_id(&comp.vendor_id);
            for key_id in &version.key_ids {
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
        }
    }

    // If auth was required but neither track produced any usable credentials,
    // fail loudly instead of building a runtime that can never authenticate.
    if auth_values.is_empty() && auth_resolve_failed && key_pool.is_none() && oauth_pool.is_none() {
        return Err(anyhow!(
            "component '{}' has no usable auth: no env/inline auth, no legacy binding keys, \
             and no enabled key on its selected version in the vendor-key store",
            component_id
        ));
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
    let mut language_map = template
        .constraints
        .as_ref()
        .map(|constraints| constraints.language_map.clone())
        .unwrap_or_default();
    if let Some(binding) = binding_entry {
        language_map.extend(binding.language_map.clone());
    }
    let oauth_manager = (!vendor_oauth.configs.is_empty()).then(|| {
        Arc::new(
            crate::component_rt::oauth::OAuthTokenManager::new(
                vendor_oauth.configs,
                oauth_transport,
                vendor_oauth_path(),
            )
            .with_recovery_db(db),
        )
    });

    Ok(ComponentRuntime {
        template,
        auth_values,
        supported_business_lines: vec![],
        language_map,
        supported_content_formats,
        supported_formats,
        key_pool,
        oauth_pool,
        oauth_manager,
        proxy_profile_id,
        runtime_max_concurrent_requests,
        runtime_min_interval_ms,
        runtime_concurrency_sem,
        runtime_last_request_at,
    })
}
