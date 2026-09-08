use super::*;

pub(in crate::web_ui::routes) async fn handle_local_components_list(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let doc = load_local_components_runtime_doc();
    let params = parse_query_string(query);
    let q = params.get("q").map(|s| s.to_lowercase());
    let kind_filter = params.get("kind").map(|s| s.to_lowercase());
    let enabled_filter =
        params
            .get("enabled")
            .and_then(|s| match s.trim().to_ascii_lowercase().as_str() {
                "1" | "true" => Some(true),
                "0" | "false" => Some(false),
                _ => None,
            });
    let page: usize = params
        .get("page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let per_page: usize = params
        .get("per_page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
        .clamp(1, 200);

    let mut items: Vec<Value> = doc
        .components
        .iter()
        .filter(|(id, comp)| {
            if let Some(enabled) = enabled_filter {
                if comp.enabled != enabled {
                    return false;
                }
            }
            if let Some(ref kf) = kind_filter {
                if comp.kind.to_lowercase() != *kf {
                    return false;
                }
            }
            if let Some(ref q) = q {
                id.to_lowercase().contains(q)
                    || comp.name.to_lowercase().contains(q)
                    || comp.template_id.to_lowercase().contains(q)
                    || comp.vendor_name.to_lowercase().contains(q)
            } else {
                true
            }
        })
        .map(|(id, comp)| {
            json!({
                "id": id,
                "name": comp.name,
                "template_id": comp.template_id,
                "source_template_id": comp.source_template_id,
                "source_template_updated_at": comp.source_template_updated_at,
                "source_template_api_version": comp.source_template_api_version,
                "vendor_id": comp.vendor_id,
                "vendor_name": comp.vendor_name,
                "kind": comp.kind,
                "capability": local_component_capability_id(&comp.kind),
                "remarks": comp.remarks,
                "enabled": comp.enabled,
                "created_at": comp.created_at,
                "updated_at": comp.updated_at,
                "component_overrides": comp.component_overrides,
                "versions": comp.versions,
            })
        })
        .collect();
    items.sort_by(|a, b| {
        a.get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("id").and_then(|v| v.as_str()).unwrap_or(""))
    });
    let total = items.len();
    let total_pages = ((total as f64) / (per_page as f64)).ceil() as usize;
    let start = (page - 1) * per_page;
    let page_items: Vec<Value> = items.into_iter().skip(start).take(per_page).collect();

    let kinds: Vec<String> = {
        let mut ks: Vec<String> = doc.components.values().map(|c| c.kind.clone()).collect();
        ks.sort();
        ks.dedup();
        ks
    };

    let payload = json!({
        "success": true,
        "data": {
            "items": page_items,
            "page": page,
            "per_page": per_page,
            "total": total,
            "total_pages": total_pages,
            "kinds": kinds,
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

pub(in crate::web_ui::routes) async fn handle_local_component_detail(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let doc = load_local_components_runtime_doc();
    let Some(comp) = doc.components.get(id) else {
        return write_not_found_response(socket, "NOT_FOUND", "component not found").await;
    };

    let payload = json!({
        "success": true,
        "data": {
            "id": id,
            "name": comp.name,
            "template_id": comp.template_id,
            "source_template_id": comp.source_template_id,
            "source_template_updated_at": comp.source_template_updated_at,
            "source_template_api_version": comp.source_template_api_version,
            "vendor_id": comp.vendor_id,
            "vendor_name": comp.vendor_name,
            "kind": comp.kind,
            "capability": local_component_capability_id(&comp.kind),
            "remarks": comp.remarks,
            "enabled": comp.enabled,
            "created_at": comp.created_at,
            "updated_at": comp.updated_at,
            "component_overrides": comp.component_overrides,
            "versions": comp.versions,
            "template_json": comp.template_json
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

pub(in crate::web_ui::routes) fn generate_openai_compatible_template(
    api_base: &str,
    model: &str,
    system_prompt: Option<&str>,
    temperature: Option<f64>,
    max_tokens: Option<u64>,
    response_path: Option<&str>,
) -> serde_json::Value {
    let url = format!("{}/v1/chat/completions", api_base.trim_end_matches('/'));
    let default_system_prompt = "You are a professional translator. Translate the following text from {{input.source_lang}} to {{input.target_lang}}. Output only the translation, nothing else. Do not add any explanations or notes.";
    let sys_prompt = system_prompt
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(default_system_prompt);
    let temp = temperature.unwrap_or(0.1);
    let resp_path = response_path
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("choices.0.message.content");

    let mut body_obj = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": sys_prompt },
            { "role": "user", "content": "{{input.text}}" }
        ],
        "temperature": temp
    });
    if let Some(mt) = max_tokens {
        if mt > 0 {
            body_obj["max_tokens"] = json!(mt);
        }
    }

    json!({
        "id": format!("openai-compat-{}", model.chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .take(64)
            .collect::<String>()),
        "name": format!("OpenAI Compatible ({})", model),
        "version": "1.0.0",
        "type": "text",
        "auth": {
            "fields": [
                { "name": "api_key", "required": true }
            ]
        },
        "request": {
            "url": url,
            "method": "POST",
            "headers": {
                "Content-Type": "application/json",
                "Authorization": "Bearer {{auth.api_key}}"
            },
            "body": body_obj
        },
        "response": {
            "translated_text_path": resp_path
        },
        "sign": null,
        "constraints": {
            "split_strategy": "paragraph",
            "supported_content_formats": ["plain_text", "rich_html", "json_structured", "serialized_php"]
        }
    })
}

struct OpenAiCompatibleTemplateConfig {
    api_base: String,
    model: String,
    system_prompt: Option<String>,
    temperature: Option<f64>,
    max_tokens: Option<u64>,
    response_path: Option<String>,
}

fn read_openai_compatible_template_config(
    template_json: Option<&serde_json::Value>,
) -> OpenAiCompatibleTemplateConfig {
    let request = template_json
        .and_then(|value| value.get("request"))
        .and_then(|value| value.as_object());
    let body = request
        .and_then(|value| value.get("body"))
        .and_then(|value| value.as_object());
    let response = template_json
        .and_then(|value| value.get("response"))
        .and_then(|value| value.as_object());
    let api_base = request
        .and_then(|value| value.get("url"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .trim_end_matches("/v1/chat/completions")
        .trim_end_matches('/')
        .to_string();
    let model = body
        .and_then(|value| value.get("model"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let system_prompt = body
        .and_then(|value| value.get("messages"))
        .and_then(|value| value.as_array())
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                let role = message.get("role").and_then(|value| value.as_str())?.trim();
                let content = message
                    .get("content")
                    .and_then(|value| value.as_str())?
                    .trim();
                if role == "system" && !content.is_empty() {
                    Some(content.to_string())
                } else {
                    None
                }
            })
        });
    let temperature = body
        .and_then(|value| value.get("temperature"))
        .and_then(|value| value.as_f64());
    let max_tokens = body
        .and_then(|value| value.get("max_tokens"))
        .and_then(|value| value.as_u64());
    let response_path = response
        .and_then(|value| value.get("translated_text_path"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    OpenAiCompatibleTemplateConfig {
        api_base,
        model,
        system_prompt,
        temperature,
        max_tokens,
        response_path,
    }
}

pub(in crate::web_ui::routes) async fn handle_local_component_create_v2(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid POST /api/components/local json payload")?;
    let id = req
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        return write_error_response(socket, "INVALID_ID", "id is required").await;
    }
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        return write_error_response(socket, "INVALID_NAME", "name is required").await;
    }
    let raw_template_id = req
        .get("template_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let mut vendor_id = req
        .get("vendor_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let vendor_name = req
        .get("vendor_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let kind = req
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("text")
        .trim()
        .to_string();
    let remarks = req
        .get("remarks")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let enabled = req.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let provided_template_json = req.get("template_json").cloned();

    if !matches!(
        kind.as_str(),
        "text" | "image" | "video" | "audio" | "document" | "openai_compatible"
    ) {
        return write_error_response(socket, "INVALID_KIND", "unsupported component kind").await;
    }
    let template_id = if kind == "openai_compatible" || provided_template_json.is_some() {
        String::new()
    } else {
        raw_template_id
    };
    if kind != "openai_compatible"
        && provided_template_json.is_none()
        && template_id.trim().is_empty()
    {
        return write_error_response(
            socket,
            "INVALID_TEMPLATE_ID",
            "template_id is required when template_json is not provided",
        )
        .await;
    }

    let template_json = if let Some(template_json) = provided_template_json {
        if !template_json.is_object() {
            return write_error_response(
                socket,
                "INVALID_TEMPLATE_JSON",
                "template_json must be an object",
            )
            .await;
        }
        if let Err(err) = serde_json::from_value::<ComponentTemplate>(template_json.clone()) {
            return write_error_response(
                socket,
                "INVALID_TEMPLATE_JSON",
                &format!("template_json is not a valid component template: {}", err),
            )
            .await;
        }
        Some(template_json)
    } else if kind == "openai_compatible" {
        let api_base = req
            .get("api_base")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let model = req
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if api_base.is_empty() {
            return write_error_response(
                socket,
                "INVALID_API_BASE",
                "api_base is required for openai_compatible",
            )
            .await;
        }
        if model.is_empty() {
            return write_error_response(
                socket,
                "INVALID_MODEL",
                "model is required for openai_compatible",
            )
            .await;
        }
        if !api_base.starts_with("http://") && !api_base.starts_with("https://") {
            return write_error_response(
                socket,
                "INVALID_API_BASE",
                "api_base must start with http:// or https://",
            )
            .await;
        }
        let system_prompt = req.get("system_prompt").and_then(|v| v.as_str());
        let temperature = req.get("temperature").and_then(|v| v.as_f64());
        let max_tokens = req.get("max_tokens").and_then(|v| v.as_u64());
        let response_path = req.get("response_path").and_then(|v| v.as_str());
        Some(generate_openai_compatible_template(
            &api_base,
            &model,
            system_prompt,
            temperature,
            max_tokens,
            response_path,
        ))
    } else {
        None
    };

    let (template_json, source_template_updated_at, source_template_api_version) =
        if let Some(template_json) = template_json {
            (Some(template_json), None, None)
        } else if !crate::config::server_control_plane_enabled() {
            // P0-LF-03 5.5: creating a non-OpenAI component without an inline
            // template never downloads a server snapshot in local mode; it is
            // a local validation error (install from the signed local catalog
            // or provide template_json instead).
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "COMPONENT_TEMPLATE_REQUIRED",
                &format!(
                    "component '{id}' needs an inline template_json in local mode; install it from the local catalog or paste a template"
                ),
            )
            .await;
        } else {
            match fetch_server_template_snapshot_for_local_component(state, &template_id, &kind)
                .await
            {
                Ok((snapshot, server_component)) => {
                    if let Some(server_vendor_id) = server_component
                        .as_ref()
                        .and_then(|component| component.vendor_id.as_deref())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        vendor_id = server_vendor_id.to_string();
                    }
                    (
                        Some(snapshot),
                        server_component
                            .as_ref()
                            .and_then(|component| component.updated_at.clone()),
                        server_component
                            .as_ref()
                            .and_then(|component| component.api_version.clone()),
                    )
                }
                Err(err) => {
                    if let Some(upstream) = err.downcast_ref::<UpstreamApiError>() {
                        let status = upstream.status.clone();
                        let code = upstream.code.clone();
                        let message = upstream.message.clone();
                        return write_error_response_with_status(socket, &status, &code, &message)
                            .await;
                    }
                    return write_error_response(
                        socket,
                        "COMPONENT_TEMPLATE_NOT_FOUND",
                        &format!("{:#}", err),
                    )
                    .await;
                }
            }
        };

    let mut doc = load_local_components_runtime_doc();
    if doc.components.contains_key(&id) {
        return write_conflict_response(socket, "DUPLICATE_ID", "component id already exists")
            .await;
    }
    let now = format!("{}", unix_ts());
    doc.components.insert(
        id.clone(),
        crate::types::ComponentInstanceLocal {
            name: name.clone(),
            template_id: template_id.clone(),
            source_template_id: if template_json.is_some() || kind == "openai_compatible" {
                String::new()
            } else {
                template_id.clone()
            },
            source_template_updated_at,
            source_template_api_version,
            vendor_id,
            vendor_name,
            kind,
            remarks,
            enabled,
            created_at: now,
            updated_at: Some(format!("{}", unix_ts())),
            component_overrides: None,
            versions: HashMap::new(),
            active_version: None,
            template_json,
        },
    );
    save_local_components_runtime_doc(&doc)?;
    let payload = json!({ "success": true, "data": { "id": id } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(in crate::web_ui::routes) async fn handle_local_component_update_v2(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    id: &str,
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid PUT /api/components/local/:id json payload")?;
    let mut doc = load_local_components_runtime_doc();
    let Some(comp) = doc.components.get_mut(id) else {
        return write_not_found_response(socket, "NOT_FOUND", "component not found").await;
    };
    let mut refresh_template_snapshot = comp.template_json.is_none();
    if req.get("enabled").and_then(|v| v.as_bool()) == Some(false) && comp.enabled {
        let usage = {
            let guard = state.lock().await;
            collect_component_usage_summary(
                id,
                &guard.task_type_component_bindings,
                &guard.rule_component_bindings,
            )
        };
        if !usage.references.is_empty() {
            return write_conflict_response(
                socket,
                "COMPONENT_IN_USE",
                &component_in_use_message(&usage),
            )
            .await;
        }
    }
    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            comp.name = trimmed.to_string();
        }
    }
    if let Some(remarks) = req.get("remarks").and_then(|v| v.as_str()) {
        comp.remarks = remarks.trim().to_string();
    }
    if let Some(enabled) = req.get("enabled").and_then(|v| v.as_bool()) {
        comp.enabled = enabled;
    }
    if comp.kind == "openai_compatible" {
        if let Some(vendor_id) = req.get("vendor_id").and_then(|v| v.as_str()) {
            comp.vendor_id = vendor_id.trim().to_string();
        }
    }
    if let Some(vendor_name) = req.get("vendor_name").and_then(|v| v.as_str()) {
        comp.vendor_name = vendor_name.trim().to_string();
    }
    if let Some(template_json) = req.get("template_json").cloned() {
        if !template_json.is_object() {
            return write_error_response(
                socket,
                "INVALID_TEMPLATE_JSON",
                "template_json must be an object",
            )
            .await;
        }
        if let Err(err) = serde_json::from_value::<ComponentTemplate>(template_json.clone()) {
            return write_error_response(
                socket,
                "INVALID_TEMPLATE_JSON",
                &format!("template_json is not a valid component template: {}", err),
            )
            .await;
        }
        refresh_template_snapshot = false;
        comp.template_id.clear();
        comp.source_template_id.clear();
        comp.source_template_updated_at = None;
        comp.source_template_api_version = None;
        comp.template_json = Some(template_json);
    } else if comp.kind == "openai_compatible" {
        refresh_template_snapshot = false;
        comp.template_id.clear();
        comp.source_template_id.clear();
        comp.source_template_updated_at = None;
        comp.source_template_api_version = None;

        let has_openai_field_update = [
            "api_base",
            "model",
            "system_prompt",
            "temperature",
            "max_tokens",
            "response_path",
        ]
        .iter()
        .any(|field| req.get(field).is_some());
        if has_openai_field_update {
            let existing = read_openai_compatible_template_config(comp.template_json.as_ref());
            let api_base = req
                .get("api_base")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or(existing.api_base);
            let model = req
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or(existing.model);
            if api_base.is_empty() || model.is_empty() {
                return write_error_response(
                    socket,
                    "INVALID_PARAMS",
                    "api_base and model are required for openai_compatible",
                )
                .await;
            }
            if !api_base.starts_with("http://") && !api_base.starts_with("https://") {
                return write_error_response(
                    socket,
                    "INVALID_API_BASE",
                    "api_base must start with http:// or https://",
                )
                .await;
            }
            let system_prompt = req
                .get("system_prompt")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .map(str::to_string)
                .or(existing.system_prompt);
            let temperature = req
                .get("temperature")
                .and_then(|v| v.as_f64())
                .or(existing.temperature);
            let max_tokens = req
                .get("max_tokens")
                .and_then(|v| v.as_u64())
                .or(existing.max_tokens);
            let response_path = req
                .get("response_path")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or(existing.response_path);
            comp.template_json = Some(generate_openai_compatible_template(
                &api_base,
                &model,
                system_prompt.as_deref(),
                temperature,
                max_tokens,
                response_path.as_deref(),
            ));
        }
    } else if let Some(template_id) = req.get("template_id").and_then(|v| v.as_str()) {
        let next_template_id = template_id.trim().to_string();
        if comp.template_id != next_template_id {
            comp.template_id = next_template_id;
            comp.source_template_id = comp.template_id.clone();
            refresh_template_snapshot = true;
        }
    }
    if comp.kind != "openai_compatible"
        && comp.template_json.is_none()
        && comp.template_id.trim().is_empty()
    {
        return write_error_response(
            socket,
            "INVALID_TEMPLATE_ID",
            "template_id is required when template_json is not provided",
        )
        .await;
    }
    // P0-LF-03 5.5: updating a component NEVER refreshes a server snapshot
    // in local mode; the local inline template stays authoritative. The
    // explicit refresh lives behind the legacy-only
    // /api/components/local/:id/refresh-snapshot route.
    if comp.kind != "openai_compatible"
        && refresh_template_snapshot
        && crate::config::server_control_plane_enabled()
    {
        match fetch_server_template_snapshot_for_local_component(
            state,
            &comp.template_id,
            &comp.kind,
        )
        .await
        {
            Ok((snapshot, server_component)) => {
                comp.template_json = Some(snapshot);
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
            }
            Err(err) => {
                if let Some(upstream) = err.downcast_ref::<UpstreamApiError>() {
                    let status = upstream.status.clone();
                    let code = upstream.code.clone();
                    let message = upstream.message.clone();
                    return write_error_response_with_status(socket, &status, &code, &message)
                        .await;
                }
                return write_error_response(
                    socket,
                    "COMPONENT_TEMPLATE_NOT_FOUND",
                    &format!("{:#}", err),
                )
                .await;
            }
        }
    }
    comp.updated_at = Some(format!("{}", unix_ts()));
    save_local_components_runtime_doc(&doc)?;
    let payload = json!({ "success": true, "data": { "id": id } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(in crate::web_ui::routes) async fn handle_local_component_delete_v2(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let usage = {
        let guard = state.lock().await;
        collect_component_usage_summary(
            id,
            &guard.task_type_component_bindings,
            &guard.rule_component_bindings,
        )
    };
    if !usage.references.is_empty() {
        return write_conflict_response(
            socket,
            "COMPONENT_IN_USE",
            &component_in_use_message(&usage),
        )
        .await;
    }

    let mut doc = load_local_components_runtime_doc();
    let removed = doc.components.remove(id).is_some();
    save_local_components_runtime_doc(&doc)?;
    let payload = json!({ "success": true, "data": { "id": id, "deleted": removed } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(in crate::web_ui::routes) async fn handle_local_component_refresh_snapshot(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    match refresh_local_component_snapshot_from_server(state, id).await {
        Ok(component) => {
            let payload = json!({
                "success": true,
                "data": {
                    "id": id,
                    "template_id": component.template_id,
                    "source_template_id": component.source_template_id,
                    "source_template_updated_at": component.source_template_updated_at,
                    "source_template_api_version": component.source_template_api_version,
                    "vendor_id": component.vendor_id,
                    "updated_at": component.updated_at,
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
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            write_error_response(
                socket,
                "COMPONENT_SNAPSHOT_REFRESH_FAILED",
                &format!("{:#}", err),
            )
            .await
        }
    }
}

// --- Export / Import / Install-from-server ---

fn public_export_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    normalized == "auth"
        || normalized == "authvalues"
        || normalized == "authdata"
        || normalized == "credentials"
        || normalized == "username"
        || normalized == "user"
        || normalized == "login"
        || normalized.contains("secret")
        || normalized.contains("token")
        || normalized.contains("password")
        || normalized.contains("apikey")
        || normalized.contains("authorization")
        || normalized.contains("privatekey")
        || normalized.contains("clientsecret")
        || normalized.contains("accesstoken")
        || normalized.contains("refreshtoken")
}

fn redact_public_export_json(value: &mut Value, path: &str, redacted_fields: &mut Vec<String>) {
    let Value::Object(fields) = value else {
        if let Value::Array(items) = value {
            for (index, item) in items.iter_mut().enumerate() {
                let item_path = if path.is_empty() {
                    index.to_string()
                } else {
                    format!("{path}[{index}]")
                };
                redact_public_export_json(item, &item_path, redacted_fields);
            }
        }
        return;
    };

    let keys: Vec<String> = fields.keys().cloned().collect();
    for key in keys {
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        if public_export_sensitive_key(&key) {
            fields.remove(&key);
            redacted_fields.push(child_path);
            continue;
        }
        if let Some(child) = fields.get_mut(&key) {
            redact_public_export_json(child, &child_path, redacted_fields);
        }
    }
}

pub(in crate::web_ui::routes) async fn handle_local_component_export(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let doc = load_local_components_runtime_doc();
    match doc.components.get(id) {
        Some(comp) => {
            // Export is public/redacted by default.  Credentials are not
            // recoverable from the browser response; a redacted component is
            // imported disabled and must be configured locally before use.
            let mut public_component = serde_json::to_value(comp)?;
            let mut redacted_fields = Vec::new();
            redact_public_export_json(&mut public_component, "component", &mut redacted_fields);
            if let Some(enabled) = public_component.get_mut("enabled") {
                *enabled = Value::Bool(false);
            }
            let export_payload = json!({
                "export_version": 2,
                "export_mode": "public",
                "redacted": true,
                "redacted_fields": redacted_fields,
                "component": public_component,
            });
            let payload = json!({ "success": true, "data": export_payload });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        None => {
            write_not_found_response(socket, "COMPONENT_NOT_FOUND", "Component not found").await
        }
    }
}

pub(in crate::web_ui::routes) async fn handle_local_component_import(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    struct ImportPayload {
        export_version: Option<u32>,
        #[serde(default)]
        redacted: bool,
        component: ComponentInstanceLocal,
        #[serde(default)]
        id: Option<String>,
    }

    let parsed: ImportPayload = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return write_error_response(
                socket,
                "INVALID_IMPORT_PAYLOAD",
                &format!("Failed to parse import data: {}", e),
            )
            .await;
        }
    };

    if let Some(version) = parsed.export_version {
        if version != 1 && version != 2 {
            return write_error_response(
                socket,
                "UNSUPPORTED_EXPORT_VERSION",
                &format!("Unsupported export version: {}", version),
            )
            .await;
        }
    }

    let mut comp = parsed.component;
    if parsed.redacted {
        // Never activate an imported public snapshot before credentials and
        // provider-specific overrides have been configured.
        comp.enabled = false;
    }
    let comp_id = parsed.id.unwrap_or_else(|| comp.template_id.clone());

    if comp_id.is_empty() {
        return write_error_response(socket, "MISSING_COMPONENT_ID", "Component ID is required")
            .await;
    }

    let mut doc = load_local_components_runtime_doc();
    let overwrite = doc.components.contains_key(&comp_id);
    doc.components.insert(comp_id.clone(), comp);
    save_local_components_runtime_doc(&doc)?;

    let payload = json!({
        "success": true,
        "data": { "id": comp_id, "overwrite": overwrite }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(in crate::web_ui::routes) async fn handle_install_server_template_to_local(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    struct InstallRequest {
        template_id: String,
        #[serde(default)]
        local_id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        remarks: Option<String>,
    }

    let req: InstallRequest = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return write_error_response(
                socket,
                "INVALID_INSTALL_REQUEST",
                &format!("Failed to parse request: {}", e),
            )
            .await;
        }
    };

    if req.template_id.is_empty() {
        return write_error_response(socket, "MISSING_TEMPLATE_ID", "template_id is required")
            .await;
    }

    // Find the server component from cached state (ComponentItem.id is the template reference)
    let server_comp = {
        let guard = state.lock().await;
        guard
            .components
            .iter()
            .find(|c| c.id == req.template_id)
            .cloned()
    };

    let server_comp = match server_comp {
        Some(c) => c,
        None => {
            return write_not_found_response(
                socket,
                "SERVER_TEMPLATE_NOT_FOUND",
                "Server template not found in cache. Try refreshing components first.",
            )
            .await;
        }
    };

    let comp_id = req.local_id.unwrap_or_else(|| server_comp.id.clone());

    let now = format!("{}", unix_ts());
    let local_comp = ComponentInstanceLocal {
        name: req.name.unwrap_or_else(|| server_comp.name.clone()),
        template_id: server_comp.id.clone(),
        source_template_id: server_comp.id.clone(),
        source_template_updated_at: server_comp.updated_at.clone(),
        source_template_api_version: server_comp.api_version.clone(),
        vendor_id: server_comp.vendor_id.clone().unwrap_or_default(),
        vendor_name: String::new(),
        kind: server_comp.kind.clone(),
        remarks: req.remarks.unwrap_or_default(),
        enabled: true,
        created_at: now.clone(),
        updated_at: Some(now),
        component_overrides: None,
        versions: HashMap::new(),
        active_version: None,
        template_json: None,
    };

    let mut doc = load_local_components_runtime_doc();
    let overwrite = doc.components.contains_key(&comp_id);
    doc.components.insert(comp_id.clone(), local_comp);
    save_local_components_runtime_doc(&doc)?;

    let payload = json!({
        "success": true,
        "data": {
            "id": comp_id,
            "overwrite": overwrite,
            "template_id": server_comp.id,
            "name": server_comp.name,
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
