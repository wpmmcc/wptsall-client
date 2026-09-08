use super::*;

pub(in crate::web_ui::routes) async fn handle_components_refresh(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let (server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return write_session_required(socket).await;
    };
    match fetch_components_for_session(&client, &server_base, &session_token).await {
        Ok(components) => {
            let (local_components_backfilled, local_backfill_error) =
                match sync_local_components_from_server(&components) {
                    Ok(count) => (count, None),
                    Err(err) => (0usize, Some(snippet(&format!("{:#}", err)))),
                };
            let mut guard = state.lock().await;
            guard.components = components;
            guard.local_components_backfilled = local_components_backfilled;
            guard.local_components_backfill_error =
                local_backfill_error.clone().unwrap_or_default();
            if let Some(ref err) = local_backfill_error {
                guard.last_error = err.clone();
                guard.last_event = "components.refreshed_with_backfill_error".to_string();
            } else {
                guard.last_error.clear();
                guard.last_event = "components.refreshed".to_string();
            }
            guard.updated_at = unix_ts();
            let payload = json!({
                "success": true,
                "data": {
                    "components": guard.components.clone(),
                    "local_components_backfilled": local_components_backfilled,
                    "local_components_backfill_error": local_backfill_error
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
        Err(err) => {
            update_state_error(state, &err, "components.refresh_failed").await;
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            write_error_response(socket, "COMPONENTS_REFRESH_FAILED", &format!("{:#}", err)).await
        }
    }
}

pub(in crate::web_ui::routes) async fn handle_components_capabilities(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    // P0-LF-03 5.2: in local mode capabilities are derived from
    // `ComponentsLocalDoc` (local components + inline templates) only. The
    // server component cache is never consulted and never leaks into the
    // local response; legacy mode keeps the server view explicitly labelled.
    if !crate::config::server_control_plane_enabled() {
        return respond_local_components_capabilities(socket).await;
    }
    let guard = state.lock().await;
    let components = &guard.components;

    let mut items: Vec<Value> = Vec::new();
    let mut format_map: HashMap<String, Vec<String>> = HashMap::new();

    for comp in components {
        items.push(json!({
            "id": comp.id,
            "name": comp.name,
            "kind": comp.kind,
            "template_group": comp.template_group,
            "size_class": comp.size_class,
            "supported_business_lines": comp.supported_business_lines,
            "supported_content_formats": comp.supported_content_formats,
            "supported_formats": comp.supported_formats,
            "client_contract": comp.client_contract,
            "constraints": {
                "max_file_size_mb": comp.max_file_size_mb,
            }
        }));

        if comp.supported_content_formats.is_empty() {
            for fmt in &[
                "plain_text",
                "rich_html",
                "json_structured",
                "serialized_php",
            ] {
                format_map
                    .entry(fmt.to_string())
                    .or_default()
                    .push(comp.id.clone());
            }
        } else {
            for fmt in &comp.supported_content_formats {
                if fmt == "media_ref" {
                    let kind_suffix = match comp.kind.as_str() {
                        "image_translation" | "image" => "image",
                        "video_translation" | "video" => "video",
                        "audio_translation" | "audio" => "audio",
                        "document_translation" | "document" => "document",
                        _ => "unknown",
                    };
                    format_map
                        .entry(format!("media_ref:{}", kind_suffix))
                        .or_default()
                        .push(comp.id.clone());
                } else {
                    format_map
                        .entry(fmt.clone())
                        .or_default()
                        .push(comp.id.clone());
                }
            }
        }
    }

    let payload = json!({
        "success": true,
        "data": {
            "source": "legacy_server",
            "components": items,
            "format_component_map": format_map,
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

/// Local-mode capabilities (P0-LF-03 5.2): derived from the local components
/// document and their inline templates only. Disabled components and
/// components without a usable local template are reported as unavailable and
/// are never routing candidates (they do not appear in
/// `format_component_map`). A missing inline template is a local data
/// problem, never a reason to contact the website.
async fn respond_local_components_capabilities(
    socket: &mut TcpStream,
) -> anyhow::Result<()> {
    let doc = crate::web_ui::routes::components::helpers::load_local_components_runtime_doc();
    let mut items: Vec<Value> = Vec::new();
    let mut format_map: HashMap<String, Vec<String>> = HashMap::new();

    for (component_id, component) in &doc.components {
        let template: Option<crate::types::ComponentTemplate> = component
            .template_json
            .clone()
            .and_then(|value| serde_json::from_value(value).ok());
        let available = component.enabled && template.is_some();
        let content_formats = match &template {
            Some(template) => {
                crate::component_rt::loader::resolve_local_supported_content_formats(
                    &component.kind,
                    template,
                )
            }
            None => Vec::new(),
        };
        items.push(json!({
            "id": component_id,
            "name": component.name,
            "kind": component.kind,
            "vendor_id": component.vendor_id,
            "vendor_name": component.vendor_name,
            "enabled": component.enabled,
            "available": available,
            "active_version": component.active_version,
            "version_count": component.versions.len(),
            "has_local_template": template.is_some(),
            "client_contract": template.as_ref().and_then(|t| t.client_contract.clone()),
            "constraints": template.as_ref().and_then(|t| t.constraints.clone()),
            "supported_content_formats": content_formats,
            "source": "local",
        }));

        // Routing candidates: enabled components with a usable local template.
        if let Some(template) = template.as_ref() {
            if component.enabled {
                for format in
                    crate::component_rt::loader::resolve_local_supported_content_formats(
                        &component.kind,
                        template,
                    )
                {
                    format_map
                        .entry(format)
                        .or_default()
                        .push(component_id.clone());
                }
            }
        }
    }

    let payload = json!({
        "success": true,
        "data": {
            "source": "local",
            "components": items,
            "format_component_map": format_map,
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

pub(in crate::web_ui::routes) async fn handle_server_components_search(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    fn status_for_server_error_code(code: &str) -> &'static str {
        match code {
            "SESSION_REQUIRED" | "SESSION_REVOKED" | "SESSION_EXPIRED" => "401 Unauthorized",
            "RATE_LIMITED" => "429 Too Many Requests",
            "COMPONENT_FORBIDDEN" | "HTTPS_REQUIRED" | "FORBIDDEN" => "403 Forbidden",
            "COMPONENT_NOT_FOUND" => "404 Not Found",
            "COMPONENT_DISABLED" | "COMPONENT_NOT_REAL_TRANSLATION" => "422 Unprocessable Entity",
            _ => "400 Bad Request",
        }
    }

    let (server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return write_session_required(socket).await;
    };

    let params = parse_query_string(query);
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    let q_val = params.get("q").cloned().unwrap_or_default();
    if !q_val.is_empty() {
        pairs.push(("q", &q_val));
    }
    let kind_val = params.get("kind").cloned().unwrap_or_default();
    if !kind_val.is_empty() {
        pairs.push(("kind", &kind_val));
    }
    let status_val = params.get("status").cloned().unwrap_or_default();
    if !status_val.is_empty() {
        pairs.push(("status", &status_val));
    }
    let page_val = params.get("page").cloned().unwrap_or_default();
    if !page_val.is_empty() {
        pairs.push(("page", &page_val));
    }
    let per_page_val = params.get("per_page").cloned().unwrap_or_default();
    if !per_page_val.is_empty() {
        pairs.push(("per_page", &per_page_val));
    }
    let bl_val = params.get("business_line").cloned().unwrap_or_default();
    if !bl_val.is_empty() {
        pairs.push(("business_line", &bl_val));
    }
    let vendor_id_val = params.get("vendor_id").cloned().unwrap_or_default();
    if !vendor_id_val.is_empty() {
        pairs.push(("vendor_id", &vendor_id_val));
    }
    let group_val = params.get("group").cloned().unwrap_or_default();
    if !group_val.is_empty() {
        pairs.push(("group", &group_val));
    }
    let content_format_val = params.get("content_format").cloned().unwrap_or_default();
    if !content_format_val.is_empty() {
        pairs.push(("content_format", &content_format_val));
    }
    let supported_type_val = params.get("supported_type").cloned().unwrap_or_default();
    if !supported_type_val.is_empty() {
        pairs.push(("supported_type", &supported_type_val));
    }
    let size_class_val = params.get("size_class").cloned().unwrap_or_default();
    if !size_class_val.is_empty() {
        pairs.push(("size_class", &size_class_val));
    }
    let product_id_val = params.get("product_id").cloned().unwrap_or_default();
    if !product_id_val.is_empty() {
        pairs.push(("product_id", &product_id_val));
    }
    let family_val = params.get("family").cloned().unwrap_or_default();
    if !family_val.is_empty() {
        pairs.push(("family", &family_val));
    }
    let subfamily_val = params.get("subfamily").cloned().unwrap_or_default();
    if !subfamily_val.is_empty() {
        pairs.push(("subfamily", &subfamily_val));
    }
    let locale_val = params.get("locale").cloned().unwrap_or_default();

    let qs: String = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter())
        .append_pair("client_type", "wpplugin")
        .finish();
    let server_url = format!("{}/api/v1/client/components?{}", server_base, qs);
    let mut headers = HeaderMap::new();
    headers.insert("X-Client-Session", session_token.parse()?);
    if !locale_val.is_empty() {
        headers.insert("X-Locale", locale_val.parse()?);
    }

    match request_json_encrypted::<Value>(
        client.get(&server_url).headers(headers),
        "server components search",
        &session_token,
    )
    .await
    {
        Ok(raw) => {
            if raw.get("success").and_then(|v| v.as_bool()) == Some(false) {
                let code = raw
                    .get("error")
                    .and_then(|v| v.get("code"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("SERVER_ERROR");
                let message = raw
                    .get("error")
                    .and_then(|v| v.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("server components search failed");
                return write_error_response_with_status(
                    socket,
                    status_for_server_error_code(code),
                    code,
                    message,
                )
                .await;
            }
            let data = if let Ok(resp) =
                serde_json::from_value::<ApiResponse<ComponentsData>>(raw.clone())
            {
                resp.data
            } else if let Ok(data) = serde_json::from_value::<ComponentsData>(raw.clone()) {
                data
            } else {
                return write_error_response_with_status(
                    socket,
                    "500 Internal Server Error",
                    "SERVER_ERROR",
                    &format!(
                        "server components search: unsupported response shape ({})",
                        raw
                    ),
                )
                .await;
            };
            let payload = json!({
                "success": true,
                "data": {
                    "items": data.items,
                    "page": data.page.unwrap_or(1),
                    "per_page": data.per_page.unwrap_or(data.items.len()),
                    "total": data.total.unwrap_or(data.items.len()),
                    "total_pages": data.total_pages.unwrap_or(1),
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
        Err(err) => {
            if let Some(result) = maybe_write_upstream_api_error(socket, &err).await {
                return result;
            }
            write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "SERVER_ERROR",
                &format!("{:#}", err),
            )
            .await
        }
    }
}
